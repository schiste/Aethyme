//! Restore a retired checkout from its recovery archive, so that reusing a
//! session whose checkout was archived and removed brings back the same
//! tree: the commits (from the archive's bundle when the branch no longer
//! holds them), the staged and the unstaged changes, the untracked files and
//! the protected ignored files, each checked against the manifest.
//!
//! The archive is verified as a whole first (every listed part present with
//! its size and SHA-256, nothing unlisted). The checkout is created by Git at
//! the archived HEAD on the session branch, the patches are applied with
//! `git apply`, and the files are copied with their permission bits (links
//! as links) and read back against their recorded SHA-256. Anything that
//! fails removes the half-made checkout again and leaves the archive as it
//! was, so a failed restore loses nothing and can be retried.
//!
//! The checkout is built at a sibling staging path
//! (`.<name>.aethyme-restoring`) and moved into place with `git worktree
//! move` only once complete, so the session's path never holds a partial
//! tree. A marker beside it (`.<name>.aethyme-restore-incomplete`) is
//! written first and removed only when the restore completed or its
//! staging checkout was removed again: while it is there, `start --reuse`
//! refuses to adopt anything at the path.
//!
//! Not restored: directory modes, modification times, hard links (each
//! linked file comes back as its own copy) and extended attributes.

use super::*;

/// What one restore put back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ArchiveRestore {
    pub head: String,
    pub bundle_used: bool,
    /// The session branch was missing and was recreated from the archive.
    pub branch_recreated: bool,
    pub staged_patch_bytes: u64,
    pub unstaged_patch_bytes: u64,
    pub untracked_files: usize,
    pub ignored_files: usize,
    /// The branch was not at the archived HEAD: the checkout is detached at
    /// the archived HEAD and this is the branch's tip.
    pub detached_branch_tip: Option<String>,
}

/// The sibling of `target` a restore builds the checkout at.
pub(crate) fn restore_staging_path(target: &Path) -> PathBuf {
    sibling(target, "aethyme-restoring")
}

/// The marker beside `target` saying a restore there has not completed.
pub(crate) fn restore_incomplete_marker(target: &Path) -> PathBuf {
    sibling(target, "aethyme-restore-incomplete")
}

/// Whether a restore at `target` started and neither completed nor cleaned
/// up after itself: nothing at `target` may be adopted then.
pub(crate) fn restore_incomplete(target: &Path) -> bool {
    std::fs::symlink_metadata(restore_incomplete_marker(target)).is_ok()
}

fn sibling(target: &Path, suffix: &str) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.{suffix}"))
}

#[cfg(test)]
thread_local! {
    /// Test hooks: fail right after the staging checkout is created, and
    /// make removing it fail.
    pub(crate) static FAIL_AFTER_ADD_FOR_TEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static FAIL_UNDO_FOR_TEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run Git in `dir`; stdout on success, the trimmed stderr otherwise.
fn git_in(dir: &Path, args: &[&std::ffi::OsStr]) -> Result<String, String> {
    let mut command = crate::git::git_command();
    command.args(args).current_dir(dir);
    match crate::bounded_output::output_limited(
        &mut command,
        crate::git::git_timeout(),
        16 * 1024 * 1024,
    ) {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) => Err(format!(
            "git {}: {}",
            args.iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(_) => Err("git did not finish within its limit".into()),
        Err(error) => Err(format!("git could not run: {error}")),
    }
}

fn os(value: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(value)
}

/// Copy one archived file (or link) from `source_root` into `target`, then
/// read it back against its recorded SHA-256.
fn restore_file(source_root: &Path, target: &Path, file: &ArchivedFile) -> Result<(), String> {
    let relative = Path::new(&file.path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("archived path {} is not relative", file.path));
    }
    let source = source_root.join(relative);
    let destination = target.join(relative);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(format!(
            "{} already exists in the restored checkout",
            file.path
        ));
    }
    let digest = if file.symlink {
        let link = std::fs::read_link(&source)
            .map_err(|error| format!("cannot read archived link {}: {error}", file.path))?;
        std::os::unix::fs::symlink(&link, &destination)
            .map_err(|error| format!("cannot restore link {}: {error}", file.path))?;
        let mut bytes = b"symlink\0".to_vec();
        bytes.extend_from_slice(link.as_os_str().as_encoded_bytes());
        sha256_hex(&bytes)
    } else {
        std::fs::copy(&source, &destination)
            .map_err(|error| format!("cannot restore {}: {error}", file.path))?;
        if let Some(mode) = file.mode {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(mode))
                .map_err(|error| format!("cannot restore the mode of {}: {error}", file.path))?;
        }
        let bytes = std::fs::read(&destination)
            .map_err(|error| format!("cannot read back {}: {error}", file.path))?;
        sha256_hex(&bytes)
    };
    if digest != file.sha256 {
        return Err(format!(
            "{} does not match its recorded SHA-256 after restore",
            file.path
        ));
    }
    Ok(())
}

/// Restore the archive at `archive` into a new checkout at `target` on
/// `branch` (created at the archived HEAD when absent). `main_root` is the
/// repository's primary checkout. See the module documentation.
pub(crate) fn restore_recovery_archive(
    main_root: &Path,
    archive: &Path,
    target: &Path,
    branch: &str,
) -> Result<ArchiveRestore, String> {
    let _span = crate::phase_telemetry::span("recovery_archive_restore");
    let manifest_bytes = read_recovery_manifest(&archive.join(MANIFEST_FILE))?;
    let manifest: StoredManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("unreadable {MANIFEST_FILE}: {error}"))?;
    verify_recovery_archive_tree(archive, &manifest)?;
    if std::fs::symlink_metadata(target).is_ok() {
        return Err(format!("{} already exists", target.display()));
    }
    let final_target = target;
    let staging = restore_staging_path(final_target);
    let marker = restore_incomplete_marker(final_target);
    if std::fs::symlink_metadata(&staging).is_ok() {
        return Err(format!(
            "an earlier restore left {}; inspect it and remove it with `git worktree remove --force` before retrying",
            staging.display()
        ));
    }
    let target = staging.as_path();
    let head = manifest.head.clone();
    let mut restored = ArchiveRestore {
        head: head.clone(),
        staged_patch_bytes: manifest.staged_patch.bytes,
        unstaged_patch_bytes: manifest.unstaged_patch.bytes,
        untracked_files: manifest.untracked_files.len(),
        ignored_files: manifest.ignored_files.as_ref().map_or(0, Vec::len),
        ..ArchiveRestore::default()
    };
    // The commits: already in the repository, or from the archive's bundle.
    let has_head = git_in(
        main_root,
        &[os("cat-file"), os("-e"), os(&format!("{head}^{{commit}}"))],
    )
    .is_ok();
    if !has_head {
        let Some(bundle) = manifest.bundle.as_deref() else {
            return Err(format!(
                "the archived HEAD {head} is not in the repository and the archive holds no bundle"
            ));
        };
        git_in(
            main_root,
            &[
                os("bundle"),
                os("unbundle"),
                archive.join(bundle).as_os_str(),
            ],
        )?;
        restored.bundle_used = true;
    }
    let branch_ref = format!("refs/heads/{branch}");
    let branch_tip = git_in(
        main_root,
        &[
            os("rev-parse"),
            os("--verify"),
            os("--quiet"),
            os(&branch_ref),
        ],
    )
    .ok();
    let add = match branch_tip.as_deref() {
        // The branch is where the checkout was: check it out there.
        Some(tip) if tip == head => git_in(
            main_root,
            &[os("worktree"), os("add"), target.as_os_str(), os(branch)],
        ),
        // The branch is gone (a lane deleted it, or someone did): recreate
        // it at the tip the archive recorded, when the repository has that
        // commit, else at the archived HEAD, then check out the archived
        // HEAD on it (detached when the recorded tip is not HEAD).
        None => {
            let recorded_tip = manifest
                .branch_tip
                .as_deref()
                .filter(|tip| {
                    git_in(
                        main_root,
                        &[os("cat-file"), os("-e"), os(&format!("{tip}^{{commit}}"))],
                    )
                    .is_ok()
                })
                .unwrap_or(head.as_str())
                .to_string();
            git_in(main_root, &[os("branch"), os(branch), os(&recorded_tip)])?;
            restored.branch_recreated = true;
            crate::phase_telemetry::add(
                "recovery_archive_restore_branch_recreated",
                Duration::ZERO,
            );
            if recorded_tip != head {
                restored.detached_branch_tip = Some(recorded_tip.clone());
            }
            if recorded_tip == head {
                git_in(
                    main_root,
                    &[os("worktree"), os("add"), target.as_os_str(), os(branch)],
                )
            } else {
                git_in(
                    main_root,
                    &[
                        os("worktree"),
                        os("add"),
                        os("--detach"),
                        target.as_os_str(),
                        os(&head),
                    ],
                )
            }
        }
        // The branch moved since: the archived HEAD, detached, as the
        // archive's restore steps describe; the branch is left alone.
        Some(tip) => {
            restored.detached_branch_tip = Some(tip.to_string());
            git_in(
                main_root,
                &[
                    os("worktree"),
                    os("add"),
                    os("--detach"),
                    target.as_os_str(),
                    os(&head),
                ],
            )
        }
    };
    if let Err(reason) = add {
        crate::phase_telemetry::add("recovery_archive_restore_failed", Duration::ZERO);
        return Err(reason);
    }
    // From here a partial tree exists (at the staging path): the marker
    // blocks adopting the session's path until it is gone.
    let marked = std::fs::write(
        &marker,
        format!(
            "aethyme: a restore of {} into {} has not completed\n",
            archive.display(),
            final_target.display()
        ),
    );
    // A failed step removes the staging checkout; the marker stays when that
    // fails too, so nothing at the session's path is adopted meanwhile.
    let undo = |reason: String| -> String {
        #[cfg(test)]
        let removed = if FAIL_UNDO_FOR_TEST.with(std::cell::Cell::get) {
            Err("removal refused by the test".to_string())
        } else {
            git_in(
                main_root,
                &[
                    os("worktree"),
                    os("remove"),
                    os("--force"),
                    target.as_os_str(),
                ],
            )
        };
        #[cfg(not(test))]
        let removed = git_in(
            main_root,
            &[
                os("worktree"),
                os("remove"),
                os("--force"),
                target.as_os_str(),
            ],
        );
        crate::phase_telemetry::add("recovery_archive_restore_failed", Duration::ZERO);
        match removed {
            Ok(_) => {
                if std::fs::symlink_metadata(&marker).is_ok() {
                    crate::warn_unrecorded(
                        "remove a restore marker",
                        std::fs::remove_file(&marker),
                    );
                }
                reason
            }
            Err(error) => {
                crate::phase_telemetry::add("recovery_archive_restore_undo_failed", Duration::ZERO);
                format!(
                    "{reason}; the partial checkout at {} could not be removed ({error}); {} blocks adopting {} until it is removed",
                    target.display(),
                    marker.display(),
                    final_target.display()
                )
            }
        }
    };
    if let Err(error) = marked {
        return Err(undo(format!("cannot write {}: {error}", marker.display())));
    }
    #[cfg(test)]
    if FAIL_AFTER_ADD_FOR_TEST.with(std::cell::Cell::get) {
        return Err(undo("failure injected by the test".into()));
    }
    if manifest.staged_patch.bytes > 0 {
        let patch = archive.join(&manifest.staged_patch.file);
        if let Err(reason) = git_in(
            target,
            &[
                os("apply"),
                os("--index"),
                os("--binary"),
                patch.as_os_str(),
            ],
        ) {
            return Err(undo(reason));
        }
    }
    if manifest.unstaged_patch.bytes > 0 {
        let patch = archive.join(&manifest.unstaged_patch.file);
        if let Err(reason) = git_in(target, &[os("apply"), os("--binary"), patch.as_os_str()]) {
            return Err(undo(reason));
        }
    }
    for file in &manifest.untracked_files {
        if let Err(reason) =
            restore_file(&archive.join(&manifest.untracked_directory), target, file)
        {
            return Err(undo(reason));
        }
    }
    if let (Some(directory), Some(files)) = (
        manifest.ignored_directory.as_deref(),
        manifest.ignored_files.as_deref(),
    ) {
        for file in files {
            if let Err(reason) = restore_file(&archive.join(directory), target, file) {
                return Err(undo(reason));
            }
        }
    }
    // Complete: move it into place (Git updates its registration).
    if let Err(reason) = git_in(
        main_root,
        &[
            os("worktree"),
            os("move"),
            target.as_os_str(),
            final_target.as_os_str(),
        ],
    ) {
        return Err(undo(reason));
    }
    crate::warn_unrecorded("remove a restore marker", std::fs::remove_file(&marker));
    if restored.detached_branch_tip.is_some() {
        crate::phase_telemetry::add("recovery_archive_restore_detached", Duration::ZERO);
    }
    crate::phase_telemetry::add("recovery_archive_restored", Duration::ZERO);
    Ok(restored)
}
