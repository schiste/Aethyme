//! Pure broker garbage-collection planning.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::broker::{
    WORKTREE_ROOT_MARKER, WorktreeRootMarker, directory_size_without_following_links,
};
use crate::{
    Broker, BrokerOpError, GcApplyReport, GcArtifactCandidate, GcBlocker, GcFileAction,
    GcFileCandidate, GcHealth, GcOrphanCandidate, GcPlan, GcRowCandidate, GcWorktreeCandidate,
    GitRepo, OperationStatus, RetentionPolicy, load_retention_policy,
};

pub const GC_PLAN_SCHEMA_VERSION: u32 = 2;

/// Git-ignored build directories that may be reclaimed independently of a
/// worktree's cleanup disposition. Each name is paired with a witness that
/// must be present before the directory is treated as a build cache, so an
/// unrelated source directory that merely shares the name is never removed.
const ARTIFACT_DIRECTORIES: &[(&str, ArtifactWitness)] = &[
    ("target", ArtifactWitness::File("CACHEDIR.TAG")),
    ("node_modules", ArtifactWitness::NonEmptyDirectory),
];

/// How deep below a worktree root a build directory is looked for. Deep enough
/// for nested workspaces and package directories, shallow enough to keep the
/// scan bounded on large trees.
const ARTIFACT_SCAN_DEPTH: usize = 6;

/// `meta` key holding the last autonomous artifact sweep time.
const ARTIFACT_SWEEP_STAMP_KEY: &str = "gc.artifact_sweep.last_run_ms";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactWitness {
    File(&'static str),
    NonEmptyDirectory,
}

impl ArtifactWitness {
    /// The entry that must outlive the rest of the removal, if there is one.
    ///
    /// A witness file classifies the directory, so taking it first turns an
    /// interrupted removal into a directory GC can no longer explain. A
    /// directory witnessed only by being non-empty needs no such care: while
    /// anything is left it still witnesses itself.
    fn deferrable_entry(self) -> Option<&'static str> {
        match self {
            Self::File(name) => Some(name),
            Self::NonEmptyDirectory => None,
        }
    }

    fn confirms(self, path: &Path) -> bool {
        match self {
            Self::File(name) => path.join(name).is_file(),
            Self::NonEmptyDirectory => std::fs::read_dir(path)
                .map(|mut entries| entries.next().is_some())
                .unwrap_or(false),
        }
    }
}

fn is_real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

fn days_between(now: i64, earlier: i64) -> u32 {
    u32::try_from(now.saturating_sub(earlier) / 86_400_000).unwrap_or(u32::MAX)
}

/// Classify a build directory found beneath `root`, if it is one.
fn artifact_witness_for(path: &Path) -> Option<ArtifactWitness> {
    let name = path.file_name()?.to_str()?;
    ARTIFACT_DIRECTORIES
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, witness)| *witness)
        .filter(|witness| witness.confirms(path))
}

/// Collect build directories beneath `root`, never descending into one that
/// already matched and never following symlinks.
fn collect_artifact_dirs(root: &Path, current: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth > ARTIFACT_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_real_directory(&path) {
            continue;
        }
        // The worktree's own git metadata is never a build artifact.
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        if artifact_witness_for(&path).is_some() {
            found.push(path);
            continue;
        }
        collect_artifact_dirs(root, &path, depth + 1, found);
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn cutoff(now: i64, days: u32) -> i64 {
    now.saturating_sub(i64::from(days).saturating_mul(86_400_000))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn retained_metric_bytes(bytes: &[u8], cutoff: i64) -> Vec<u8> {
    let mut after = Vec::with_capacity(bytes.len());
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let timestamp = serde_json::from_slice::<serde_json::Value>(line)
            .ok()
            .and_then(|value| value.get("ts").and_then(serde_json::Value::as_i64));
        if !timestamp.is_some_and(|timestamp| timestamp < cutoff) {
            after.extend_from_slice(line);
        }
    }
    after
}

fn repo_relative(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

fn metrics_rewrite(
    main_root: &Path,
    cutoff: i64,
    blockers: &mut Vec<GcBlocker>,
) -> Result<Option<GcFileCandidate>, BrokerOpError> {
    let relative = ".aethyme/logs/command-metrics.jsonl";
    let path = main_root.join(relative);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            blockers.push(GcBlocker {
                kind: "command_metrics".into(),
                id: None,
                reason: format!("cannot inspect {relative}: {error}"),
            });
            return Ok(None);
        }
    };
    if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        blockers.push(GcBlocker {
            kind: "command_metrics".into(),
            id: None,
            reason: format!("{relative} is a symlink and is never rewritten by GC"),
        });
        return Ok(None);
    }

    let after = retained_metric_bytes(&bytes, cutoff);
    let mut removed = false;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let timestamp = serde_json::from_slice::<serde_json::Value>(line)
            .ok()
            .and_then(|value| value.get("ts").and_then(serde_json::Value::as_i64));
        match timestamp {
            Some(timestamp) if timestamp < cutoff => removed = true,
            Some(_) => {}
            None => {
                blockers.push(GcBlocker {
                    kind: "command_metric_line".into(),
                    id: None,
                    reason: "retained one malformed or timestamp-free command metric line".into(),
                });
            }
        }
    }
    if !removed {
        return Ok(None);
    }
    Ok(Some(GcFileCandidate {
        path: relative.into(),
        action: GcFileAction::Rewrite,
        before_sha256: sha256(&bytes),
        after_sha256: Some(sha256(&after)),
        bytes_before: bytes.len() as u64,
        bytes_after: after.len() as u64,
        source_row_ids: Vec::new(),
    }))
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct GcJournal {
    schema_version: u32,
    digest: String,
    evaluated_at: i64,
    policy: RetentionPolicy,
    remaining_rows: Vec<GcRowCandidate>,
    remaining_files: Vec<GcFileCandidate>,
    remaining_worktrees: Vec<GcWorktreeCandidate>,
    #[serde(default)]
    remaining_artifacts: Vec<GcArtifactCandidate>,
    #[serde(default)]
    remaining_orphans: Vec<GcOrphanCandidate>,
    rows_removed: usize,
    files_completed: Vec<String>,
    sessions_cleaned: Vec<i64>,
    #[serde(default)]
    artifacts_reclaimed: Vec<String>,
    #[serde(default)]
    orphans_removed: Vec<String>,
    reclaimed_bytes: u64,
}

impl From<GcPlan> for GcJournal {
    fn from(plan: GcPlan) -> Self {
        Self {
            schema_version: plan.schema_version,
            digest: plan.digest,
            evaluated_at: plan.evaluated_at,
            policy: plan.policy,
            remaining_rows: plan.rows,
            remaining_files: plan.files,
            remaining_worktrees: plan.worktrees,
            remaining_artifacts: plan.artifacts,
            remaining_orphans: plan.orphans,
            rows_removed: 0,
            files_completed: Vec::new(),
            sessions_cleaned: Vec::new(),
            artifacts_reclaimed: Vec::new(),
            orphans_removed: Vec::new(),
            reclaimed_bytes: 0,
        }
    }
}

struct GcLock {
    path: PathBuf,
}

impl GcLock {
    fn acquire(main_root: &Path) -> Result<Self, BrokerOpError> {
        let path = main_root.join(".aethyme/gc.lock");
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
            {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id()).map_err(|source| {
                        crate::BrokerError::Io {
                            path: path.clone(),
                            source,
                        }
                    })?;
                    file.sync_all().map_err(|source| crate::BrokerError::Io {
                        path: path.clone(),
                        source,
                    })?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let owner = std::fs::read_to_string(&path).unwrap_or_default();
                    let pid = owner.trim().parse::<i64>().ok();
                    if pid.is_some_and(crate::broker::pid_alive) {
                        return Err(BrokerOpError::GcLocked {
                            pid: owner.trim().to_owned(),
                        });
                    }
                    std::fs::remove_file(&path).map_err(|source| crate::BrokerError::Io {
                        path: path.clone(),
                        source,
                    })?;
                }
                Err(source) => {
                    return Err(crate::BrokerError::Io {
                        path: path.clone(),
                        source,
                    }
                    .into());
                }
            }
        }
        Err(BrokerOpError::GcLocked {
            pid: "unknown".into(),
        })
    }
}

impl Drop for GcLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), BrokerOpError> {
    let parent = path
        .parent()
        .ok_or_else(|| BrokerOpError::InvalidGcJournal {
            reason: format!("path has no parent: {}", path.display()),
        })?;
    std::fs::create_dir_all(parent).map_err(|source| crate::BrokerError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        now_ms()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok::<_, std::io::Error>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|source| crate::BrokerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn write_journal(path: &Path, journal: &GcJournal) -> Result<(), BrokerOpError> {
    let mut bytes = serde_json::to_vec_pretty(journal)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

fn load_journal(path: &Path) -> Result<Option<GcJournal>, BrokerOpError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(crate::BrokerError::Io {
                path: path.to_path_buf(),
                source,
            }
            .into());
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| BrokerOpError::InvalidGcJournal {
            reason: error.to_string(),
        })
}

fn runtime_path(main_root: &Path, relative: &str) -> Result<PathBuf, BrokerOpError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || !relative.starts_with(".aethyme/")
    {
        return Err(BrokerOpError::InvalidGcJournal {
            reason: format!("unsafe runtime path {relative:?}"),
        });
    }
    Ok(main_root.join(path))
}

fn check_deadline(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

/// How many times a directory whose `rmdir` raced a writer is swept again
/// before the failure is reported. macOS writes `.DS_Store` into directories
/// as they are browsed, so a removal that takes minutes can find a file in a
/// directory it already emptied. The second sweep has nothing left to walk and
/// costs milliseconds, which is far too short a window to lose again.
const TREE_REMOVAL_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeRemoval {
    /// The directory is gone.
    Complete,
    /// The budget ran out. The directory is still there, still carrying the
    /// evidence that classifies it, and the next pass resumes it.
    Interrupted,
}

/// Remove a condemned directory tree without ever losing the evidence that it
/// was condemned.
///
/// Two properties `std::fs::remove_dir_all` does not have, both of which this
/// code path needs:
///
/// * **Resumable.** Removing a multi-gigabyte `target/` takes minutes, which
///   no budget on a broker-open path can absorb, and a deadline checked only
///   between whole directories is no deadline at all. Entries go one at a
///   time, and every call removes at least one of them before the deadline is
///   consulted, so a budget too small to be worth spending still buys ground.
///
///   What makes an interrupted removal safe to resume is `keep_until_last`:
///   the entry that proves what this directory is -- `CACHEDIR.TAG` for a
///   cargo target, the root marker for an orphaned worktree root -- is taken
///   only once everything else is gone. Remove it first and an interrupted
///   run leaves an unclassifiable directory that GC must then refuse to
///   touch, which is how a half-finished removal becomes permanent.
///
/// * **Tolerant of a racing writer.** Finder and Spotlight write `.DS_Store`
///   into directories while they are being walked, so the final `rmdir` of a
///   long removal fails `ENOTEMPTY` against a file created after that
///   directory was already emptied. A directory that refuses to go is swept
///   again rather than aborting the caller's whole run.
fn remove_condemned_tree(
    dir: &Path,
    keep_until_last: Option<&str>,
    deadline: Option<Instant>,
) -> std::io::Result<TreeRemoval> {
    let mut attempts_left = TREE_REMOVAL_ATTEMPTS;
    loop {
        attempts_left -= 1;
        if !is_real_directory(dir) {
            return Ok(TreeRemoval::Complete);
        }
        if drain_directory(dir, keep_until_last, deadline)? == TreeRemoval::Interrupted {
            return Ok(TreeRemoval::Interrupted);
        }
        if let Some(name) = keep_until_last {
            remove_entry(&dir.join(name), false)?;
        }
        match std::fs::remove_dir(dir) {
            Ok(()) => return Ok(TreeRemoval::Complete),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TreeRemoval::Complete);
            }
            Err(error) if attempts_left == 0 => return Err(error),
            // Something reappeared underneath while we walked. Sweep again.
            Err(_) => {}
        }
    }
}

/// Empty one directory depth-first, leaving the directory itself in place.
///
/// `keep_until_last` is skipped at this level only; the recursion never defers
/// anything, because only the top directory carries the witness.
fn drain_directory(
    dir: &Path,
    keep_until_last: Option<&str>,
    deadline: Option<Instant>,
) -> std::io::Result<TreeRemoval> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TreeRemoval::Complete);
        }
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if keep_until_last.is_some_and(|name| entry.file_name() == std::ffi::OsStr::new(name)) {
            continue;
        }
        let path = entry.path();
        // `DirEntry::file_type` does not follow symlinks, so a link to a
        // directory is unlinked rather than followed and emptied.
        let is_directory = match entry.file_type() {
            Ok(file_type) => file_type.is_dir(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if is_directory {
            if drain_directory(&path, None, deadline)? == TreeRemoval::Interrupted {
                return Ok(TreeRemoval::Interrupted);
            }
        }
        remove_entry(&path, is_directory)?;
        // Checked after the entry, never before: a pass that removes nothing
        // is a pass that will be repeated forever. Budgets small enough to be
        // spent on the scan alone are the normal case on a broker open, so
        // every call has to be worth at least one unlink.
        if check_deadline(deadline) {
            return Ok(TreeRemoval::Interrupted);
        }
    }
    Ok(TreeRemoval::Complete)
}

/// Unlink one entry, treating "already gone" as the outcome we wanted.
///
/// A nested directory that a writer refilled is left alone: it resurfaces as
/// the parent's `ENOTEMPTY`, which is what the retry in
/// [`remove_condemned_tree`] exists to absorb. Reporting it here would abort a
/// removal that is one cheap sweep away from finishing.
fn remove_entry(path: &Path, is_directory: bool) -> std::io::Result<()> {
    let removed = if is_directory {
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) if is_directory && error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

impl Broker {
    /// Build directories inside retained worktrees, for sessions idle at least
    /// `artifact_reclaim_days`.
    ///
    /// This deliberately ignores the worktree's cleanup disposition. Blocked
    /// dispositions protect *commits*; build caches hold none, so a worktree
    /// whose provenance is unproven still has reclaimable bytes. Sessions
    /// already scheduled for whole-worktree removal are skipped so the two
    /// candidate sets never double-count the same bytes.
    fn artifact_candidates(
        &self,
        evaluated_at: i64,
        policy: &RetentionPolicy,
        cleanup: &[crate::CleanupWorktreePlan],
        sessions: &BTreeMap<i64, crate::Session>,
        already_removed: &[i64],
    ) -> Vec<GcArtifactCandidate> {
        let mut candidates = Vec::new();
        for item in cleanup {
            if !item.worktree_present || already_removed.contains(&item.session_id) {
                continue;
            }
            // Only closed sessions appear here; a live worktree is still in use.
            let Some(session) = sessions.get(&item.session_id) else {
                continue;
            };
            let closed_at = session.closed_at.unwrap_or(session.updated_at);
            let idle_days = days_between(evaluated_at, closed_at);
            if idle_days < policy.artifact_reclaim_days {
                continue;
            }
            let root = PathBuf::from(&item.worktree_path);
            if !is_real_directory(&root) {
                continue;
            }
            let Ok(checkout) = GitRepo::discover(&root) else {
                continue;
            };
            let mut found = Vec::new();
            collect_artifact_dirs(&root, &root, 0, &mut found);
            for dir in found {
                let Some(relative) = repo_relative(&root, &dir) else {
                    continue;
                };
                if !checkout.path_is_ignored(&relative) {
                    continue;
                }
                let bytes = directory_size_without_following_links(&dir).unwrap_or(0);
                if bytes == 0 {
                    continue;
                }
                candidates.push(GcArtifactCandidate {
                    session_id: item.session_id,
                    worktree_path: item.worktree_path.clone(),
                    relative_dir: relative,
                    estimated_bytes: bytes,
                    idle_days,
                });
            }
        }
        candidates.sort_by(|left, right| {
            (left.session_id, &left.relative_dir).cmp(&(right.session_id, &right.relative_dir))
        });
        candidates
    }

    /// Host worktree roots whose owning repository is gone.
    ///
    /// Each root carries a `.aethyme-worktree-root.json` breadcrumb naming the
    /// repository that created it. When that repository no longer exists, no
    /// broker database can ever account for the tree again, so nothing but a
    /// host-level sweep will reclaim it.
    fn orphan_candidates(
        &self,
        evaluated_at: i64,
        policy: &RetentionPolicy,
        blockers: &mut Vec<GcBlocker>,
    ) -> Result<Vec<GcOrphanCandidate>, BrokerOpError> {
        let plan = self.worktree_root_plan()?;
        let Some(container) = plan.root_container.clone() else {
            return Ok(Vec::new());
        };
        let Ok(entries) = std::fs::read_dir(&container) else {
            return Ok(Vec::new());
        };
        let mut candidates = Vec::new();
        for entry in entries.flatten() {
            let root = entry.path();
            if !is_real_directory(&root) {
                continue;
            }
            let key = root.file_name().unwrap_or_default().to_string_lossy();
            if key == plan.repository_key {
                continue;
            }
            let marker_path = root.join(WORKTREE_ROOT_MARKER);
            let Ok(bytes) = std::fs::read(&marker_path) else {
                // No breadcrumb means no provable owner; never remove blind.
                blockers.push(GcBlocker {
                    kind: "unmarked_worktree_root".into(),
                    id: None,
                    reason: format!("{key} has no {WORKTREE_ROOT_MARKER} and is never swept"),
                });
                continue;
            };
            let Ok(marker) = serde_json::from_slice::<WorktreeRootMarker>(&bytes) else {
                blockers.push(GcBlocker {
                    kind: "unmarked_worktree_root".into(),
                    id: None,
                    reason: format!("{key} has an unreadable {WORKTREE_ROOT_MARKER}"),
                });
                continue;
            };
            if marker.repository_root.exists() {
                continue;
            }
            let age_days = std::fs::metadata(&root)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|since| days_between(evaluated_at, since.as_millis() as i64))
                .unwrap_or(0);
            if age_days < policy.orphan_worktree_roots_days {
                blockers.push(GcBlocker {
                    kind: "orphan_grace".into(),
                    id: None,
                    reason: format!(
                        "orphaned root {key} is younger than the {} day grace period",
                        policy.orphan_worktree_roots_days
                    ),
                });
                continue;
            }
            candidates.push(GcOrphanCandidate {
                repository_key: marker.repository_key,
                worktree_root: root.to_string_lossy().into_owned(),
                repository_root: marker.repository_root.to_string_lossy().into_owned(),
                estimated_bytes: directory_size_without_following_links(&root).unwrap_or(0),
                reason: "owning repository no longer exists".into(),
            });
        }
        candidates.sort_by(|left, right| left.worktree_root.cmp(&right.worktree_root));
        Ok(candidates)
    }

    pub fn gc_plan(&mut self) -> Result<GcPlan, BrokerOpError> {
        let evaluated_at = now_ms();
        let main_root = self.main_root().to_path_buf();
        let policy = load_retention_policy(&main_root)?;
        let cleanup = self.cleanup_plan()?;
        let sessions = self
            .store()
            .cleaned_sessions()?
            .into_iter()
            .map(|session| (session.id, session))
            .collect::<BTreeMap<_, _>>();
        let mut rows = self.store().gc_row_candidates(
            cutoff(evaluated_at, policy.terminal_events_days),
            cutoff(evaluated_at, policy.gate_results_days),
            cutoff(evaluated_at, policy.terminal_merge_queue_days),
        )?;
        let mut blockers = Vec::new();
        for session in sessions.values() {
            if let Some(queue_entry_id) = session.accepted_queue_entry_id {
                blockers.push(GcBlocker {
                    kind: "accepted_checkpoint".into(),
                    id: Some(queue_entry_id),
                    reason: format!(
                        "session {} still names this queue entry as accepted provenance",
                        session.id
                    ),
                });
            }
        }
        for session in self.store().live_sessions()? {
            blockers.push(GcBlocker {
                kind: "live_session".into(),
                id: Some(session.id),
                reason: "live sessions and their rows are never aged out".into(),
            });
        }
        for advisory in self.store().advisories(false)? {
            blockers.push(GcBlocker {
                kind: "outstanding_advisory".into(),
                id: Some(advisory.id),
                reason: "outstanding and acknowledged advisories remain authoritative".into(),
            });
        }
        for exposure in self.store().outstanding_entry_path_exposures()? {
            blockers.push(GcBlocker {
                kind: "publication_exposure".into(),
                id: Some(exposure.id),
                reason: "publication has not been verified".into(),
            });
        }
        for operation in self.store().coordinated_operations()? {
            if matches!(
                operation.status,
                OperationStatus::Prepared
                    | OperationStatus::Running
                    | OperationStatus::OutcomeUnknown
            ) {
                blockers.push(GcBlocker {
                    kind: "unresolved_operation".into(),
                    id: Some(operation.id),
                    reason: "unresolved external outcome remains write-blocking".into(),
                });
            }
        }

        let gate_root = main_root.join(".aethyme/logs/gates");
        let mut files = BTreeMap::<String, GcFileCandidate>::new();
        let mut retained_rows = Vec::with_capacity(rows.len());
        for row in rows.drain(..) {
            let Some(log) = row.gate_log_path.as_deref() else {
                retained_rows.push(row);
                continue;
            };
            let path = PathBuf::from(log);
            if !path.exists() {
                retained_rows.push(row);
                continue;
            }
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    blockers.push(GcBlocker {
                        kind: "gate_log".into(),
                        id: Some(row.id),
                        reason: format!("cannot inspect gate log: {error}"),
                    });
                    continue;
                }
            };
            let Some(relative) = repo_relative(&main_root, &path) else {
                blockers.push(GcBlocker {
                    kind: "gate_log".into(),
                    id: Some(row.id),
                    reason: "gate result names a log outside the repository runtime directory"
                        .into(),
                });
                continue;
            };
            if !path.starts_with(&gate_root)
                || metadata.file_type().is_symlink()
                || !metadata.is_file()
            {
                blockers.push(GcBlocker {
                    kind: "gate_log".into(),
                    id: Some(row.id),
                    reason: format!("{relative} is not a regular broker-owned gate log"),
                });
                continue;
            }
            let bytes = std::fs::read(&path).map_err(|source| crate::BrokerError::Io {
                path: path.clone(),
                source,
            })?;
            files
                .entry(relative.clone())
                .and_modify(|file| file.source_row_ids.push(row.id))
                .or_insert_with(|| GcFileCandidate {
                    path: relative,
                    action: GcFileAction::Delete,
                    before_sha256: sha256(&bytes),
                    after_sha256: None,
                    bytes_before: bytes.len() as u64,
                    bytes_after: 0,
                    source_row_ids: vec![row.id],
                });
            retained_rows.push(row);
        }
        rows = retained_rows;

        if let Some(metrics) = metrics_rewrite(
            &main_root,
            cutoff(evaluated_at, policy.command_metrics_days),
            &mut blockers,
        )? {
            files.insert(metrics.path.clone(), metrics);
        }

        let worktree_cutoff = cutoff(evaluated_at, policy.closed_worktrees_days);
        let mut worktrees = Vec::new();
        for item in cleanup.worktrees.iter().cloned() {
            let Some(session) = sessions.get(&item.session_id) else {
                continue;
            };
            let closed_at = session.closed_at.unwrap_or(session.updated_at);
            if !item.eligible() {
                blockers.push(GcBlocker {
                    kind: "unproven_contribution".into(),
                    id: Some(item.session_id),
                    reason: item.reason,
                });
                continue;
            }
            if closed_at >= worktree_cutoff {
                blockers.push(GcBlocker {
                    kind: "retention_age".into(),
                    id: Some(item.session_id),
                    reason: format!(
                        "closed worktree is younger than the {} day policy",
                        policy.closed_worktrees_days
                    ),
                });
                continue;
            }
            worktrees.push(GcWorktreeCandidate {
                session_id: item.session_id,
                worktree_path: item.worktree_path,
                worktree_present: item.worktree_present,
                branch_ref: item.branch_ref,
                branch_tip: item.branch_tip,
                estimated_bytes: item.estimated_bytes.unwrap_or(0),
                closed_at,
            });
        }

        let removed_sessions = worktrees
            .iter()
            .map(|worktree| worktree.session_id)
            .collect::<Vec<_>>();
        let artifacts = self.artifact_candidates(
            evaluated_at,
            &policy,
            &cleanup.worktrees,
            &sessions,
            &removed_sessions,
        );
        let orphans = self.orphan_candidates(evaluated_at, &policy, &mut blockers)?;

        rows.sort_by_key(|row| (row.kind, row.id));
        let mut files = files.into_values().collect::<Vec<_>>();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        worktrees.sort_by_key(|worktree| worktree.session_id);
        blockers.sort_by(|left, right| {
            (&left.kind, left.id, &left.reason).cmp(&(&right.kind, right.id, &right.reason))
        });
        blockers.dedup();
        let estimated_reclaimable_bytes = rows
            .iter()
            .map(|row| row.estimated_bytes)
            .chain(
                files
                    .iter()
                    .map(|file| file.bytes_before.saturating_sub(file.bytes_after)),
            )
            .chain(worktrees.iter().map(|worktree| worktree.estimated_bytes))
            .chain(artifacts.iter().map(|artifact| artifact.estimated_bytes))
            .chain(orphans.iter().map(|orphan| orphan.estimated_bytes))
            .fold(0_u64, u64::saturating_add);
        // Retained bytes describe disk pressure, not authorized work: every
        // byte held by a retained worktree plus every orphaned host root.
        let estimated_retained_bytes = orphans
            .iter()
            .map(|orphan| orphan.estimated_bytes)
            .fold(cleanup.estimated_retained_bytes, u64::saturating_add);
        // Blocked bytes compare like with like: only worktree-scoped totals.
        // Row and file candidates live in the repository runtime directory and
        // were never counted as retained, so subtracting them would understate
        // what policy is actually holding back.
        let worktree_scoped_reclaimable = worktrees
            .iter()
            .map(|worktree| worktree.estimated_bytes)
            .chain(artifacts.iter().map(|artifact| artifact.estimated_bytes))
            .chain(orphans.iter().map(|orphan| orphan.estimated_bytes))
            .fold(0_u64, u64::saturating_add);
        let estimated_blocked_bytes =
            estimated_retained_bytes.saturating_sub(worktree_scoped_reclaimable);
        let mut plan = GcPlan {
            schema_version: GC_PLAN_SCHEMA_VERSION,
            digest: String::new(),
            evaluated_at,
            policy,
            rows,
            files,
            worktrees,
            artifacts,
            orphans,
            blockers,
            estimated_reclaimable_bytes,
            estimated_retained_bytes,
            estimated_blocked_bytes,
        };
        plan.finish_digest()?;
        Ok(plan)
    }

    pub fn gc_apply(&mut self, confirm: &str) -> Result<GcApplyReport, BrokerOpError> {
        self.gc_apply_bounded(confirm, None)
    }

    pub fn gc_health(&mut self) -> Result<GcHealth, BrokerOpError> {
        let plan = self.gc_plan()?;
        let journal = load_journal(&self.main_root().join(".aethyme/gc-journal.json"))?;
        let over_retained_bytes_budget = plan.policy.retained_bytes_budget > 0
            && plan.estimated_retained_bytes >= plan.policy.retained_bytes_budget;
        Ok(GcHealth {
            policy: plan.policy,
            pending_recovery_digest: journal.map(|journal| journal.digest),
            candidate_rows: plan.rows.len(),
            candidate_files: plan.files.len(),
            candidate_worktrees: plan.worktrees.len(),
            candidate_artifacts: plan.artifacts.len(),
            candidate_orphans: plan.orphans.len(),
            estimated_reclaimable_bytes: plan.estimated_reclaimable_bytes,
            estimated_retained_bytes: plan.estimated_retained_bytes,
            estimated_blocked_bytes: plan.estimated_blocked_bytes,
            over_retained_bytes_budget,
            blockers: plan.blockers.len(),
        })
    }

    /// Continue only a GC plan that an operator already authorized, then run
    /// the autonomous artifact sweep.
    ///
    /// Startup never invents or confirms a plan that removes committed work.
    /// The artifact sweep is exempt from that rule because it only removes
    /// git-ignored build caches, which hold no contribution and are recovered
    /// by rebuilding.
    pub(crate) fn resume_gc_maintenance(&mut self) -> Result<Option<GcApplyReport>, BrokerOpError> {
        let policy = load_retention_policy(self.main_root())?;
        let journal_path = self.main_root().join(".aethyme/gc-journal.json");
        let resumed = match load_journal(&journal_path)? {
            Some(journal) => {
                Some(self.gc_apply_bounded(&journal.digest, Some(policy.startup_budget_ms))?)
            }
            None => None,
        };
        let _ = self.sweep_artifacts_autonomously(&policy);
        Ok(resumed)
    }

    /// Reclaim build caches from long-idle closed worktrees without operator
    /// confirmation.
    ///
    /// Deliberately avoids [`Broker::cleanup_plan`]: sizing every retained
    /// worktree is a full stat walk over tens of gigabytes, far too expensive
    /// for a path that runs on every broker open. Discovery here is a bounded
    /// `read_dir` scan and sizes are never computed; the operator-invoked
    /// `gc plan` remains the surface that reports bytes.
    ///
    /// The budget bounds the work, not the number of directories attempted.
    /// A single `target/` can hold several gigabytes and take minutes to
    /// unlink, which no per-open budget can absorb in one go, so removal stops
    /// at the deadline mid-directory and leaves the tree still classified as
    /// the build cache it is. An unfinished pass withholds the cadence stamp,
    /// so the next broker open resumes instead of waiting out the interval,
    /// and a backlog that cannot fit in one budget still drains.
    fn sweep_artifacts_autonomously(
        &mut self,
        policy: &RetentionPolicy,
    ) -> Result<usize, BrokerOpError> {
        if policy.artifact_sweep_budget_ms == 0 {
            return Ok(0);
        }
        let main_root = self.main_root().to_path_buf();
        let now = now_ms();
        let interval_ms = i64::from(policy.artifact_sweep_interval_hours) * 3_600_000;
        if let Some(last) = self
            .store()
            .meta_get(ARTIFACT_SWEEP_STAMP_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            && now.saturating_sub(last) < interval_ms
        {
            return Ok(0);
        }
        // A concurrent GC owns the artifact namespace; skip rather than race.
        let Ok(_lock) = GcLock::acquire(&main_root) else {
            return Ok(0);
        };
        let live = self
            .store()
            .live_sessions()?
            .into_iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        let deadline = Instant::now() + Duration::from_millis(policy.artifact_sweep_budget_ms);
        let mut removed = Vec::new();
        let mut eligible_worktree_seen = false;
        let mut scan_completed = true;
        for session in self.store().cleaned_sessions()? {
            if live.contains(&session.id) {
                continue;
            }
            let closed_at = session.closed_at.unwrap_or(session.updated_at);
            if days_between(now, closed_at) < policy.artifact_reclaim_days {
                continue;
            }
            let root = PathBuf::from(&session.worktree_path);
            if !is_real_directory(&root) || !self.is_broker_owned_worktree(&session, &root) {
                continue;
            }
            let Ok(checkout) = GitRepo::discover(&root) else {
                continue;
            };
            eligible_worktree_seen = true;
            let mut found = Vec::new();
            collect_artifact_dirs(&root, &root, 0, &mut found);
            for dir in found {
                let Some(relative) = repo_relative(&root, &dir) else {
                    continue;
                };
                if !checkout.path_is_ignored(&relative) {
                    continue;
                }
                let deferrable =
                    artifact_witness_for(&dir).and_then(ArtifactWitness::deferrable_entry);
                match remove_condemned_tree(&dir, deferrable, Some(deadline)) {
                    Ok(TreeRemoval::Complete) => {
                        removed.push(dir.to_string_lossy().into_owned());
                    }
                    // The budget stopped a removal partway. The directory is
                    // still a recognisable build cache, so the next pass finds
                    // it again and carries on -- and withholding the cadence
                    // stamp below is what makes a next pass happen today
                    // rather than after the interval.
                    Ok(TreeRemoval::Interrupted) => scan_completed = false,
                    Err(_) => {}
                }
                if check_deadline(Some(deadline)) {
                    scan_completed = false;
                }
                if !scan_completed {
                    break;
                }
            }
            if !scan_completed {
                break;
            }
        }
        // Do not consume the cadence window before any closed worktree exists,
        // and do not hide an unfinished backlog for a full interval. Removal
        // failures do consume the window, preventing a broken path from
        // slowing every broker command until an operator can inspect it.
        if eligible_worktree_seen && scan_completed {
            self.store()
                .meta_set(ARTIFACT_SWEEP_STAMP_KEY, &now.to_string())?;
        }
        if !removed.is_empty() {
            let payload = serde_json::json!({
                "directories": removed.len(),
                "idle_days": policy.artifact_reclaim_days,
            })
            .to_string();
            self.store().append_event(
                crate::events::BROKER_GC_ARTIFACTS_SWEPT,
                None,
                Some(&payload),
            )?;
        }
        Ok(removed.len())
    }

    /// Apply or resume an authorized plan. A budget is used by amortized
    /// maintenance; `None` runs until completion or a concrete artifact
    /// failure. Progress is journaled after every bounded batch/item.
    pub fn gc_apply_bounded(
        &mut self,
        confirm: &str,
        budget_ms: Option<u64>,
    ) -> Result<GcApplyReport, BrokerOpError> {
        if confirm.len() != 64 || !confirm.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BrokerOpError::GcConfirmationNotSha256);
        }
        let main_root = self.main_root().to_path_buf();
        let _lock = GcLock::acquire(&main_root)?;
        let journal_path = main_root.join(".aethyme/gc-journal.json");
        let mut journal = match load_journal(&journal_path)? {
            Some(journal) => {
                if journal.schema_version != GC_PLAN_SCHEMA_VERSION {
                    return Err(BrokerOpError::InvalidGcJournal {
                        reason: format!(
                            "schema {} is unsupported (expected {})",
                            journal.schema_version, GC_PLAN_SCHEMA_VERSION
                        ),
                    });
                }
                if !journal.digest.eq_ignore_ascii_case(confirm) {
                    return Err(BrokerOpError::GcResumeConfirmationMismatch {
                        expected: journal.digest,
                        actual: confirm.to_owned(),
                    });
                }
                journal
            }
            None => {
                let plan = self.gc_plan()?;
                if !plan.digest.eq_ignore_ascii_case(confirm) {
                    return Err(BrokerOpError::GcConfirmationMismatch {
                        actual: confirm.to_owned(),
                    });
                }
                let journal = GcJournal::from(plan);
                write_journal(&journal_path, &journal)?;
                journal
            }
        };
        let deadline = budget_ms.map(|budget| Instant::now() + Duration::from_millis(budget));
        let mut failures = Vec::new();

        while !journal.remaining_rows.is_empty() && !check_deadline(deadline) {
            let count = journal.remaining_rows.len().min(128);
            let batch = journal.remaining_rows[..count].to_vec();
            self.store().delete_gc_rows(&batch)?;
            journal.rows_removed = journal.rows_removed.saturating_add(batch.len());
            journal.reclaimed_bytes = journal.reclaimed_bytes.saturating_add(
                batch
                    .iter()
                    .map(|row| row.estimated_bytes)
                    .fold(0_u64, u64::saturating_add),
            );
            journal.remaining_rows.drain(..count);
            write_journal(&journal_path, &journal)?;
        }

        while !journal.remaining_files.is_empty() && !check_deadline(deadline) {
            let candidate = journal.remaining_files[0].clone();
            let path = runtime_path(&main_root, &candidate.path)?;
            let current = match std::fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    failures.push(format!("{}: {error}", candidate.path));
                    break;
                }
            };
            let completed = match (candidate.action, current) {
                (GcFileAction::Delete, None) => true,
                (GcFileAction::Delete, Some(bytes)) => {
                    let observed = sha256(&bytes);
                    if observed != candidate.before_sha256 {
                        return Err(BrokerOpError::GcArtifactDrift {
                            path: candidate.path,
                            expected: candidate.before_sha256,
                            actual: observed,
                        });
                    }
                    match std::fs::remove_file(&path) {
                        Ok(()) => true,
                        Err(error) => {
                            failures.push(format!("{}: {error}", candidate.path));
                            false
                        }
                    }
                }
                (GcFileAction::Rewrite, None) => {
                    return Err(BrokerOpError::GcArtifactDrift {
                        path: candidate.path,
                        expected: candidate.before_sha256,
                        actual: "missing".into(),
                    });
                }
                (GcFileAction::Rewrite, Some(bytes)) => {
                    let observed = sha256(&bytes);
                    let expected_after = candidate.after_sha256.as_deref().ok_or_else(|| {
                        BrokerOpError::InvalidGcJournal {
                            reason: format!(
                                "rewrite candidate {} has no after digest",
                                candidate.path
                            ),
                        }
                    })?;
                    if observed == expected_after {
                        true
                    } else if observed == candidate.before_sha256 {
                        let after = retained_metric_bytes(
                            &bytes,
                            cutoff(journal.evaluated_at, journal.policy.command_metrics_days),
                        );
                        let actual_after = sha256(&after);
                        if actual_after != expected_after {
                            return Err(BrokerOpError::GcArtifactDrift {
                                path: candidate.path,
                                expected: expected_after.to_owned(),
                                actual: actual_after,
                            });
                        }
                        match atomic_write(&path, &after) {
                            Ok(()) => true,
                            Err(error) => {
                                failures.push(format!("{}: {error}", candidate.path));
                                false
                            }
                        }
                    } else {
                        return Err(BrokerOpError::GcArtifactDrift {
                            path: candidate.path,
                            expected: candidate.before_sha256,
                            actual: observed,
                        });
                    }
                }
            };
            if !completed {
                break;
            }
            journal.reclaimed_bytes = journal
                .reclaimed_bytes
                .saturating_add(candidate.bytes_before.saturating_sub(candidate.bytes_after));
            journal.files_completed.push(candidate.path);
            journal.remaining_files.remove(0);
            write_journal(&journal_path, &journal)?;
        }

        while !journal.remaining_worktrees.is_empty() && !check_deadline(deadline) {
            let candidate = journal.remaining_worktrees[0].clone();
            let current = self
                .cleanup_plan()?
                .worktrees
                .into_iter()
                .find(|item| item.session_id == candidate.session_id);
            if let Some(current) = current {
                let exact = current.eligible()
                    && current.worktree_path == candidate.worktree_path
                    && current.branch_ref == candidate.branch_ref
                    && current.branch_tip == candidate.branch_tip;
                if !exact {
                    failures.push(format!(
                        "session {} cleanup provenance changed; review a new GC plan",
                        candidate.session_id
                    ));
                    break;
                }
                if let Err(error) = self.cleanup(candidate.session_id, false) {
                    failures.push(format!("session {}: {error}", candidate.session_id));
                    break;
                }
            }
            journal.reclaimed_bytes = journal
                .reclaimed_bytes
                .saturating_add(candidate.estimated_bytes);
            journal.sessions_cleaned.push(candidate.session_id);
            journal.remaining_worktrees.remove(0);
            write_journal(&journal_path, &journal)?;
        }

        let live = self
            .store()
            .live_sessions()?
            .into_iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        while !journal.remaining_artifacts.is_empty() && !check_deadline(deadline) {
            let candidate = journal.remaining_artifacts[0].clone();
            let root = PathBuf::from(&candidate.worktree_path);
            let dir = root.join(&candidate.relative_dir);
            // A journal naming a path outside the worktree it claims is one
            // nothing may act on -- but refusing to remove it is the whole of
            // the refusal. Carrying it forward would pin the journal, and a
            // pinned journal is a digest no `gc plan` can reproduce and no
            // command can release.
            if !Path::new(&candidate.relative_dir)
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
                || !dir.starts_with(&root)
            {
                failures.push(format!(
                    "{}: escapes its worktree and was not removed; review a new GC plan",
                    candidate.relative_dir
                ));
                journal.remaining_artifacts.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            }
            // Every other precondition is re-proven per item, because a
            // session may have been reused and rebuilt since the plan was
            // authorized -- and one candidate that no longer qualifies is
            // retained while the run carries on. These are git-ignored build
            // caches whose candidates share no fate; stopping the run on the
            // first of them is what let a single stray file leave a GC that
            // could not be finished at all.
            if !is_real_directory(&dir) {
                // Gone is the outcome this candidate asked for. No bytes are
                // claimed: this run is not what freed them.
                journal.remaining_artifacts.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            }
            if live.contains(&candidate.session_id) {
                failures.push(format!(
                    "{}: session {} is live again",
                    candidate.relative_dir, candidate.session_id
                ));
                journal.remaining_artifacts.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            }
            let Some(witness) = artifact_witness_for(&dir) else {
                failures.push(format!(
                    "{}: no longer a reclaimable build directory; review a new GC plan",
                    candidate.relative_dir
                ));
                journal.remaining_artifacts.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            };
            match remove_condemned_tree(&dir, witness.deferrable_entry(), deadline) {
                Ok(TreeRemoval::Complete) => {}
                // Still present and still classified, so the resume that
                // follows takes it from where this one stopped.
                Ok(TreeRemoval::Interrupted) => break,
                Err(error) => {
                    failures.push(format!("{}: {error}", candidate.relative_dir));
                    journal.remaining_artifacts.remove(0);
                    write_journal(&journal_path, &journal)?;
                    continue;
                }
            }
            journal.reclaimed_bytes = journal
                .reclaimed_bytes
                .saturating_add(candidate.estimated_bytes);
            journal
                .artifacts_reclaimed
                .push(dir.to_string_lossy().into_owned());
            journal.remaining_artifacts.remove(0);
            write_journal(&journal_path, &journal)?;
        }

        while !journal.remaining_orphans.is_empty() && !check_deadline(deadline) {
            let candidate = journal.remaining_orphans[0].clone();
            let root = PathBuf::from(&candidate.worktree_root);
            if !is_real_directory(&root) {
                journal.remaining_orphans.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            }
            // The owning repository reappearing revokes the whole premise --
            // for this root, and for no other, so the run continues.
            let safe = root.join(WORKTREE_ROOT_MARKER).is_file()
                && !Path::new(&candidate.repository_root).exists();
            if !safe {
                failures.push(format!(
                    "{}: orphan evidence changed; review a new GC plan",
                    candidate.worktree_root
                ));
                journal.remaining_orphans.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            }
            // The root marker is the orphan evidence, so it goes last for the
            // same reason a build cache's witness does.
            match remove_condemned_tree(&root, Some(WORKTREE_ROOT_MARKER), deadline) {
                Ok(TreeRemoval::Complete) => {}
                Ok(TreeRemoval::Interrupted) => break,
                Err(error) => {
                    failures.push(format!("{}: {error}", candidate.worktree_root));
                    journal.remaining_orphans.remove(0);
                    write_journal(&journal_path, &journal)?;
                    continue;
                }
            }
            journal.reclaimed_bytes = journal
                .reclaimed_bytes
                .saturating_add(candidate.estimated_bytes);
            journal.orphans_removed.push(candidate.worktree_root);
            journal.remaining_orphans.remove(0);
            write_journal(&journal_path, &journal)?;
        }

        let deadline_reached = check_deadline(deadline)
            && (!journal.remaining_rows.is_empty()
                || !journal.remaining_files.is_empty()
                || !journal.remaining_worktrees.is_empty()
                || !journal.remaining_artifacts.is_empty()
                || !journal.remaining_orphans.is_empty());
        let complete = journal.remaining_rows.is_empty()
            && journal.remaining_files.is_empty()
            && journal.remaining_worktrees.is_empty()
            && journal.remaining_artifacts.is_empty()
            && journal.remaining_orphans.is_empty();
        let recovery_action =
            (!complete).then(|| format!("aethyme broker gc apply --confirm {}", journal.digest));
        let report = GcApplyReport {
            digest: journal.digest.clone(),
            complete,
            deadline_reached,
            rows_removed: journal.rows_removed,
            files_completed: journal.files_completed.clone(),
            sessions_cleaned: journal.sessions_cleaned.clone(),
            artifacts_reclaimed: journal.artifacts_reclaimed.clone(),
            orphans_removed: journal.orphans_removed.clone(),
            reclaimed_bytes: journal.reclaimed_bytes,
            failures,
            recovery_action,
        };
        if complete {
            let payload = serde_json::json!({
                "digest": report.digest,
                "rows_removed": report.rows_removed,
                "files_completed": report.files_completed.len(),
                "sessions_cleaned": report.sessions_cleaned.len(),
                "artifacts_reclaimed": report.artifacts_reclaimed.len(),
                "orphans_removed": report.orphans_removed.len(),
                "reclaimed_bytes": report.reclaimed_bytes,
            })
            .to_string();
            self.store()
                .append_event(crate::events::BROKER_GC_APPLIED, None, Some(&payload))?;
            std::fs::remove_file(&journal_path).map_err(|source| crate::BrokerError::Io {
                path: journal_path,
                source,
            })?;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(root: &Path, relative: &str) -> PathBuf {
        let path = root.join(relative);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn build_directories_need_a_witness_before_they_are_reclaimable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // A cargo target directory is only recognised once cargo has stamped it.
        let target = dir(root, "rust/target");
        assert!(artifact_witness_for(&target).is_none());
        std::fs::write(target.join("CACHEDIR.TAG"), "Signature\n").unwrap();
        assert!(artifact_witness_for(&target).is_some());

        // An empty node_modules holds nothing worth reclaiming.
        let modules = dir(root, "web/node_modules");
        assert!(artifact_witness_for(&modules).is_none());
        dir(root, "web/node_modules/left-pad");
        assert!(artifact_witness_for(&modules).is_some());

        // A source directory that merely shares the name is never a candidate.
        let source = dir(root, "src/target");
        std::fs::write(source.join("main.rs"), "fn main() {}\n").unwrap();
        assert!(artifact_witness_for(&source).is_none());
    }

    #[test]
    fn collection_skips_git_metadata_and_never_descends_into_a_match() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let target = dir(root, "rust/target");
        std::fs::write(target.join("CACHEDIR.TAG"), "Signature\n").unwrap();
        // A nested match inside an outer match must not be reported twice.
        let nested = dir(root, "rust/target/debug/node_modules");
        dir(root, "rust/target/debug/node_modules/pkg");
        assert!(artifact_witness_for(&nested).is_some());

        // Git metadata is never treated as a build cache.
        let git_target = dir(root, ".git/target");
        std::fs::write(git_target.join("CACHEDIR.TAG"), "Signature\n").unwrap();

        let mut found = Vec::new();
        collect_artifact_dirs(root, root, 0, &mut found);
        assert_eq!(found, vec![target]);
    }

    #[test]
    fn collection_is_depth_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let deep = "a/b/c/d/e/f/g/h/target";
        let target = dir(root, deep);
        std::fs::write(target.join("CACHEDIR.TAG"), "Signature\n").unwrap();

        let mut found = Vec::new();
        collect_artifact_dirs(root, root, 0, &mut found);
        assert!(
            found.is_empty(),
            "a directory below the scan depth must stay untouched: {found:?}"
        );
    }

    #[test]
    fn symlinked_build_directories_are_never_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let real = dir(root, "elsewhere/target");
        std::fs::write(real.join("CACHEDIR.TAG"), "Signature\n").unwrap();

        let worktree = dir(root, "worktree");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("elsewhere"), worktree.join("linked")).unwrap();

        // A symlinked entry inside the tree is never descended into, so the
        // build directory it points at stays untouched.
        let mut found = Vec::new();
        collect_artifact_dirs(&worktree, &worktree, 0, &mut found);
        assert!(found.is_empty(), "symlinked entries must not be scanned");

        // A symlinked worktree root is rejected one layer up, by the guard
        // every caller applies before scanning.
        #[cfg(unix)]
        {
            let linked_root = root.join("linked-root");
            std::os::unix::fs::symlink(root.join("elsewhere"), &linked_root).unwrap();
            assert!(!is_real_directory(&linked_root));
        }
    }

    /// Build a `target/` holding `files` files spread over subdirectories,
    /// large enough that no plausible deadline removes it in one pass.
    fn populated_target(root: &Path, files: usize) -> PathBuf {
        let target = dir(root, "rust/target");
        std::fs::write(target.join("CACHEDIR.TAG"), "Signature\n").unwrap();
        for index in 0..files {
            let bucket = dir(&target, &format!("debug/deps/{}", index % 16));
            std::fs::write(bucket.join(format!("{index}.rlib")), b"artifact").unwrap();
        }
        target
    }

    #[test]
    fn an_interrupted_removal_stays_the_build_cache_it_was() {
        let tmp = tempfile::tempdir().unwrap();
        let target = populated_target(tmp.path(), 3000);
        let witness = artifact_witness_for(&target).unwrap().deferrable_entry();

        // A budget that expires mid-tree is the normal case on a broker open,
        // not an error: what must hold is that every observable intermediate
        // state is still recognisable as the same candidate. Lose that and a
        // removal too large for one budget can never be resumed -- which is
        // exactly how a half-deleted tree became permanent.
        let mut passes = 0;
        loop {
            let deadline = Some(Instant::now() + Duration::from_millis(2));
            let outcome = remove_condemned_tree(&target, witness, deadline).unwrap();
            passes += 1;
            assert!(passes < 10_000, "removal made no progress");
            if outcome == TreeRemoval::Complete {
                break;
            }
            assert!(
                artifact_witness_for(&target).is_some(),
                "an interrupted removal must leave a directory GC can still classify"
            );
        }
        assert!(!target.exists());
    }

    #[test]
    fn removing_what_is_already_gone_is_the_outcome_not_a_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("rust/target");
        assert_eq!(
            remove_condemned_tree(&missing, Some("CACHEDIR.TAG"), None).unwrap(),
            TreeRemoval::Complete
        );
    }

    #[test]
    fn a_directory_refilled_while_it_was_being_emptied_is_swept_again() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = dir(tmp.path(), "target/debug");
        std::fs::write(nested.join(".DS_Store"), "finder\n").unwrap();

        // Finder writes into directories as they are walked, so a directory
        // can gain a file after the removal already emptied it. Reporting
        // that as an error is what aborted the run; it is left for the retry
        // that follows to collect.
        assert!(remove_entry(&nested, true).is_ok());
        assert!(nested.exists());
        assert!(remove_entry(&tmp.path().join("target/absent"), false).is_ok());

        // The retry is what actually finishes it.
        assert_eq!(
            remove_condemned_tree(&tmp.path().join("target"), None, None).unwrap(),
            TreeRemoval::Complete
        );
    }

    #[test]
    fn a_symlinked_entry_is_unlinked_rather_than_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = dir(tmp.path(), "outside");
        std::fs::write(outside.join("keep.txt"), "keep\n").unwrap();
        let target = dir(tmp.path(), "target");
        std::fs::write(target.join("CACHEDIR.TAG"), "Signature\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, target.join("linked")).unwrap();

        assert_eq!(
            remove_condemned_tree(&target, Some("CACHEDIR.TAG"), None).unwrap(),
            TreeRemoval::Complete
        );
        assert!(
            outside.join("keep.txt").exists(),
            "following a symlink out of the tree would delete unrelated files"
        );
    }
}
