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

/// Why staging is not removed.
#[derive(Debug)]
pub(super) enum Unproven {
    /// Established: the staging holds something the checkout does not.
    Fact(String),
    /// Nothing was established (an I/O or Git failure, no time left): the
    /// attempt ends and a later one asks again.
    Operational(String),
}

impl std::fmt::Display for Unproven {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fact(reason) | Self::Operational(reason) => formatter.write_str(reason),
        }
    }
}

/// For `map_err`: an I/O failure while doing `what`.
fn operational(what: String) -> impl FnOnce(std::io::Error) -> Unproven {
    move |error| Unproven::Operational(format!("{what}: {error}"))
}

/// The files and directories of a staging directory, relative to it; every
/// directory comes before the directories in it.
#[derive(Debug, Default)]
pub(super) struct StagingTree {
    files: std::collections::BTreeSet<PathBuf>,
    directories: Vec<PathBuf>,
}

/// How long settling staging this apply does not use may take.
const SPARE_STAGING_BUDGET: Duration = Duration::from_secs(10);
/// It is not started with less time than this.
const SPARE_STAGING_MINIMUM: Duration = Duration::from_secs(1);

/// The staging name this apply uses, after removing earlier staging of the
/// same checkout that holds nothing the checkout does not. The caller holds
/// the session's retirement lock, so nothing is writing any of it.
pub(super) fn settle_retirement_staging(
    repository: &GitRepo,
    archive_root: &Path,
    plan: &CleanupResolvePlan,
    deadline: Option<Instant>,
) -> Result<String, String> {
    let names = retirement_staging_names(plan.session_id);
    let mut kept = Vec::new();
    for (index, name) in names.iter().enumerate() {
        match settle_one(repository, &archive_root.join(name), plan, deadline, false)? {
            Staging::Use => {
                settle_spare_staging(
                    repository,
                    archive_root,
                    plan,
                    deadline,
                    &names[index + 1..],
                );
                return Ok(name.clone());
            }
            Staging::Kept(reason) => kept.push(format!("{name}: {reason}")),
        }
    }
    Err(format!(
        "staging of earlier states of this checkout was retained in both staging places: {}. Both are plain copies under the archive root: copy back what only they hold from untracked/ and ignored/ and remove the directory, or archive the checkout without them with `aethyme broker finish cleanup resolve {} --archive`",
        kept.join("; "),
        plan.session_id
    ))
}

/// Remove what is proven in staging this apply does not use. Best effort: a
/// failure keeps it and changes nothing for the apply.
pub(super) fn settle_spare_staging(
    repository: &GitRepo,
    archive_root: &Path,
    plan: &CleanupResolvePlan,
    deadline: Option<Instant>,
    names: &[String],
) {
    let Some(deadline) = spare_deadline(Instant::now(), effective_archive_deadline(deadline))
    else {
        return;
    };
    for name in names {
        let _ = settle_one(
            repository,
            &archive_root.join(name),
            plan,
            Some(deadline),
            true,
        );
    }
}

/// When settling spare staging has to stop, if it may start: the apply's own
/// reserve is never spent on it.
pub(super) fn spare_deadline(now: Instant, operation: Option<Instant>) -> Option<Instant> {
    let budget = now + SPARE_STAGING_BUDGET;
    let deadline = match operation {
        Some(operation) => operation
            .checked_sub(AUTO_COMPACTION_STAGING_RESERVE)?
            .min(budget),
        None => budget,
    };
    (deadline.saturating_duration_since(now) >= SPARE_STAGING_MINIMUM).then_some(deadline)
}

fn settle_one(
    repository: &GitRepo,
    partial: &Path,
    plan: &CleanupResolvePlan,
    deadline: Option<Instant>,
    spare: bool,
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
        if remove_only_a_cut_short_record(partial) {
            count("recovery_archive_stale_partial_removed");
        }
        return Ok(Staging::Use);
    };
    if record.schema_version != PINNED_RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION
        || record.session_id != plan.session_id
        || record.worktree_path != plan.worktree_path
        || record.branch_ref != plan.branch_ref
        // The apply verifies or restarts it; spare staging it never opens.
        || (!spare && verify_retirement_archive_record_for_content_identity(partial, plan).is_ok())
    {
        return Ok(Staging::Use);
    }

    let _span = crate::phase_telemetry::span("recovery_archive_apply_stale_partial");
    let removed = prove_recorded_tips_held(repository, &record, plan, deadline)
        .and_then(|()| prove_partial_held_by_checkout(partial, plan, deadline))
        .and_then(|tree| {
            let git_deadline = proof_git_deadline(deadline)?;
            let refs = record.prerequisite_refs.as_deref().unwrap_or_default();
            if !list_recovery_prerequisite_refs(repository, refs, git_deadline)
                .map_err(|reason| {
                    if reason == MALFORMED_PREREQUISITE_NAMESPACE {
                        Unproven::Fact(reason)
                    } else {
                        Unproven::Operational(reason)
                    }
                })?
                .is_empty()
            {
                return Err(Unproven::Fact(
                    "one or more of its prerequisite refs exist".into(),
                ));
            }
            ensure_auto_retirement_staging_reserve(
                if spare { None } else { deadline },
                plan.session_id,
            )
            .map_err(|error| Unproven::Operational(error.to_string()))?;
            remove_proven_partial(partial, &tree, deadline)
        });
    match removed {
        Ok(()) => {
            count("recovery_archive_stale_partial_removed");
            Ok(Staging::Use)
        }
        Err(Unproven::Fact(reason)) => {
            count("recovery_archive_stale_partial_kept");
            Ok(Staging::Kept(reason))
        }
        Err(Unproven::Operational(reason)) => {
            count("recovery_archive_stale_partial_undecided");
            Err(format!(
                "earlier staging could not be judged and was retained: {reason}"
            ))
        }
    }
}

fn count(name: &'static str) {
    crate::phase_telemetry::add(name, Duration::ZERO);
}

fn check_time(deadline: Option<Instant>) -> Result<(), Unproven> {
    check_archive_deadline(deadline, "earlier staging proof").map_err(Unproven::Operational)
}

fn proof_git_deadline(deadline: Option<Instant>) -> Result<Instant, Unproven> {
    let git_deadline = recovery_bundle_deadline().map_err(Unproven::Operational)?;
    Ok(effective_archive_deadline(deadline)
        .map_or(git_deadline, |deadline| deadline.min(git_deadline)))
}

/// Remove a staging directory whose only entry is a record cut short while it
/// was written (or empty): nothing is copied before the record is complete.
fn remove_only_a_cut_short_record(partial: &Path) -> bool {
    let record = partial.join(RETIREMENT_ARCHIVE_RECORD_FILE);
    let Ok(entries) = std::fs::read_dir(partial) else {
        return false;
    };
    let mut entries = entries.take(2);
    if !matches!(entries.next(), Some(Ok(entry)) if entry.path() == record)
        || entries.next().is_some()
    {
        return false;
    }
    let Ok(bytes) = read_regular_file_no_follow(&record, MAX_RETIREMENT_ARCHIVE_RECORD_BYTES)
    else {
        return false;
    };
    let cut_short =
        serde_json::from_slice::<serde::de::IgnoredAny>(&bytes).is_err_and(|error| error.is_eof());
    cut_short && std::fs::remove_file(&record).is_ok() && std::fs::remove_dir(partial).is_ok()
}

/// Every commit the record names is one the plan archives, or in its history:
/// otherwise the staging may be all that says what the checkout held.
pub(super) fn prove_recorded_tips_held(
    repository: &GitRepo,
    record: &RetirementArchiveRecord,
    plan: &CleanupResolvePlan,
    deadline: Option<Instant>,
) -> Result<(), Unproven> {
    let planned = [
        Some(plan.head.as_str()),
        plan.branch_tip.as_deref(),
        plan.checkout_branch_tip.as_deref(),
    ];
    let recorded = [
        Some(record.head.as_str()),
        record.branch_tip.as_deref(),
        record.checkout_branch_tip.as_deref(),
    ];
    for tip in recorded.into_iter().flatten() {
        check_time(deadline)?;
        if planned.contains(&Some(tip)) {
            continue;
        }
        let mut command = crate::git::git_command();
        command
            .args(["cat-file", "--batch-check"])
            .current_dir(repository.root())
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1");
        let output = run_recovery_git_output_with_input_limited(
            &mut command,
            format!("{tip}\n").as_bytes(),
            proof_git_deadline(deadline)?,
            "recorded staging commit inspection",
        )
        .map_err(Unproven::Operational)?;
        if output.stdout.ends_with(b" missing\n") {
            return Err(Unproven::Fact(
                "its record names a commit this repository no longer has".into(),
            ));
        }
        let _git_deadline = crate::git::limit_git_until(proof_git_deadline(deadline)?);
        let mut held = false;
        for root in planned.into_iter().flatten() {
            held = held
                || repository
                    .is_ancestor_checked(tip, root)
                    .map_err(|error| Unproven::Operational(error.to_string()))?;
        }
        if !held {
            return Err(Unproven::Fact(
                "its record names a commit outside the history the plan archives".into(),
            ));
        }
    }
    Ok(())
}

fn staging_tree(root: &Path, deadline: Option<Instant>) -> Result<StagingTree, Unproven> {
    let mut tree = StagingTree::default();
    let mut pending = vec![(PathBuf::new(), 0_usize)];
    let mut visited = 0_usize;
    while let Some((relative, depth)) = pending.pop() {
        if depth > MAX_RECOVERY_ARCHIVE_TREE_DEPTH {
            return Err(Unproven::Fact("it is nested too deep to compare".into()));
        }
        let directory = root.join(&relative);
        let unreadable = || format!("cannot read {}", directory.display());
        let entries = std::fs::read_dir(&directory).map_err(operational(unreadable()))?;
        for entry in entries {
            check_time(deadline)?;
            visited += 1;
            if visited > MAX_RECOVERY_ARCHIVE_TREE_ENTRIES {
                return Err(Unproven::Fact(
                    "it holds too many entries to compare".into(),
                ));
            }
            let entry = entry.map_err(operational(unreadable()))?;
            let kind = entry.file_type().map_err(operational(unreadable()))?;
            let path = relative.join(entry.file_name());
            if kind.is_dir() {
                tree.directories.push(path.clone());
                pending.push((path, depth + 1));
            } else if kind.is_file() || kind.is_symlink() {
                tree.files.insert(path);
            } else {
                return Err(Unproven::Fact("it holds a special file".into()));
            }
        }
    }
    Ok(tree)
}

/// The staging's tree, once each file is proven to hold nothing the checkout
/// does not hold at a path `plan` archives. A manifest, a commit bundle, or
/// anything else the checkout's files cannot be compared with is a fact
/// against it.
pub(super) fn prove_partial_held_by_checkout(
    partial: &Path,
    plan: &CleanupResolvePlan,
    deadline: Option<Instant>,
) -> Result<StagingTree, Unproven> {
    let by_path = |files: &[ArchivedFile]| {
        files
            .iter()
            .map(|file| (file.path.clone(), file.symlink))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let untracked = by_path(&plan.untracked_files);
    let ignored = by_path(&plan.ignored_files);
    let worktree = Path::new(&plan.worktree_path);
    let tree = staging_tree(partial, deadline)?;
    // Every name first, so one that decides is not found after reading all
    // the content before it.
    let mut copies = Vec::new();
    for relative in &tree.files {
        let text = relative
            .to_str()
            .ok_or_else(|| Unproven::Fact("it holds a non-UTF-8 name".into()))?;
        let planned = match text.split_once('/') {
            None if text == RETIREMENT_ARCHIVE_RECORD_FILE => continue,
            None if text == STAGED_PATCH_FILE || text == UNSTAGED_PATCH_FILE => {
                copies.push((text, None));
                continue;
            }
            Some((UNTRACKED_DIRECTORY, path)) => (path, untracked.get(path)),
            Some((IGNORED_DIRECTORY, path)) => (path, ignored.get(path)),
            _ => {
                return Err(Unproven::Fact(format!(
                    "it holds {text}, which cannot be compared with the checkout"
                )));
            }
        };
        let (path, Some(symlink)) = planned else {
            return Err(Unproven::Fact(format!(
                "it holds {text}, which the current plan does not archive"
            )));
        };
        copies.push((text, Some((path, *symlink))));
    }
    for (text, planned) in copies {
        check_time(deadline)?;
        let copy = partial.join(text);
        match planned {
            None => {
                let (bytes, sha256) = if text == STAGED_PATCH_FILE {
                    (plan.staged_patch_bytes, &plan.staged_patch_sha256)
                } else {
                    (plan.unstaged_patch_bytes, &plan.unstaged_patch_sha256)
                };
                let held = describe_untracked_until(&copy, text, deadline)
                    .map_err(operational(format!("cannot read its {text}")))?;
                if !held.is_some_and(|held| {
                    !held.symlink && held.bytes == bytes && held.sha256 == *sha256
                }) {
                    return Err(Unproven::Fact(format!(
                        "its {text} is not the checkout's current one"
                    )));
                }
            }
            Some((path, symlink)) => {
                prove_copy_held(&copy, text, worktree, path, symlink, deadline)?;
            }
        }
    }
    Ok(tree)
}

/// Whether the checkout's file at `path` starts with everything `copy` holds
/// (a copy cut short is a prefix of its source; a source that only grew still
/// starts with it). A link must be the same link. Follows no link.
fn prove_copy_held(
    copy: &Path,
    name: &str,
    worktree: &Path,
    path: &str,
    symlink: bool,
    deadline: Option<Instant>,
) -> Result<(), Unproven> {
    let fact = |reason: &str| Unproven::Fact(format!("its {name} {reason}"));
    if !is_contained_relative(path) {
        return Err(fact("is not a contained path"));
    }
    let in_checkout = |path: &Path| match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Err(fact("is no longer in the checkout"))
        }
        Err(error) => Err(operational(format!("cannot inspect {name}"))(error)),
    };
    // A parent that became a link would compare with a file outside the
    // checkout.
    let mut parent = worktree.to_path_buf();
    for component in Path::new(path).components() {
        if !in_checkout(&parent)?.is_dir() {
            return Err(fact("is not under a real directory in the checkout"));
        }
        parent.push(component);
    }
    let source = worktree.join(path);
    let source_type = in_checkout(&source)?;
    let copy_type = std::fs::symlink_metadata(copy)
        .map_err(operational(format!("cannot inspect its {name}")))?
        .file_type();
    if copy_type.is_symlink() != symlink
        || source_type.is_symlink() != symlink
        || !(symlink || copy_type.is_file() && source_type.is_file())
    {
        return Err(fact("is another kind of file than the checkout's"));
    }
    if symlink {
        let held =
            std::fs::read_link(copy).map_err(operational(format!("cannot read its {name}")))?;
        let current =
            std::fs::read_link(&source).map_err(operational(format!("cannot read {name}")))?;
        return if held == current {
            Ok(())
        } else {
            Err(fact("is another link in the checkout"))
        };
    }
    let (mut copy_file, _) = open_regular_file_no_follow(copy)
        .map_err(operational(format!("cannot open its {name}")))?;
    let (mut source_file, _) = open_regular_file_no_follow(&source)
        .map_err(operational(format!("cannot open {name} in the checkout")))?;
    let mut held = [0_u8; 64 * 1024];
    let mut current = [0_u8; 64 * 1024];
    loop {
        check_time(deadline)?;
        let read = copy_file
            .read(&mut held)
            .map_err(operational(format!("cannot read its {name}")))?;
        if read == 0 {
            return Ok(());
        }
        match source_file.read_exact(&mut current[..read]) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(fact("is not wholly in the checkout"));
            }
            Err(error) => {
                return Err(operational(format!("cannot read {name} in the checkout"))(
                    error,
                ));
            }
        }
        if held[..read] != current[..read] {
            return Err(fact("differs from the checkout"));
        }
    }
}

/// Remove exactly the proven files, then the directories they leave empty.
/// The record goes last, so a removal that stops can be proven and resumed; a
/// file nobody proved makes a directory removal fail and stays.
pub(super) fn remove_proven_partial(
    partial: &Path,
    tree: &StagingTree,
    deadline: Option<Instant>,
) -> Result<(), Unproven> {
    let gained = || Unproven::Fact("it gained a file while it was removed".into());
    let remove_directory = |path: &Path| {
        std::fs::remove_dir(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::DirectoryNotEmpty {
                gained()
            } else {
                operational("cannot remove a proven directory".into())(error)
            }
        })
    };
    let record = Path::new(RETIREMENT_ARCHIVE_RECORD_FILE);
    for relative in tree.files.iter().filter(|path| path.as_path() != record) {
        check_time(deadline)?;
        std::fs::remove_file(partial.join(relative))
            .map_err(operational("cannot remove a proven file".into()))?;
    }
    for directory in tree.directories.iter().rev() {
        check_time(deadline)?;
        remove_directory(&partial.join(directory))?;
    }
    let left = std::fs::read_dir(partial)
        .map_err(operational("cannot read the emptied staging".into()))?
        .take(2)
        .count();
    if left != 1 {
        return Err(gained());
    }
    // The record must not be lost before the removals it vouches for last.
    sync_archive_directory(partial)
        .map_err(operational("cannot sync the emptied staging".into()))?;
    std::fs::remove_file(partial.join(record))
        .map_err(operational("cannot remove the staging record".into()))?;
    remove_directory(partial)
}
