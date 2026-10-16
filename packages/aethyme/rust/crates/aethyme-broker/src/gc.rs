//! Pure broker garbage-collection planning.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::artifact_catalog::ArtifactWitness;
use crate::broker::{
    WORKTREE_ROOT_MARKER, WorktreeRootMarker, directory_size_without_following_links,
};
use crate::{
    Broker, BrokerOpError, GcApplyReport, GcArtifactCandidate, GcBlocker, GcBlockerSummary,
    GcCheckpointPinRelease, GcDeclinedArtifact, GcFileAction, GcFileCandidate,
    GcGateCacheCandidate, GcHealth, GcOrphanCandidate, GcPlan, GcPublicationExposureExpiry,
    GcRecoveryArchiveCandidate, GcRowCandidate, GcWorktreeBlockerSummary, GcWorktreeCandidate,
    GitRepo, OperationStatus, RecoveryArchiveInventory, RetentionPolicy, load_retention_policy,
    load_retention_policy_report,
};

pub const GC_PLAN_SCHEMA_VERSION: u32 = 2;

/// How deep below a worktree root a build directory is looked for. Deep enough
/// for nested workspaces and package directories, shallow enough to keep the
/// scan bounded on large trees.
const ARTIFACT_SCAN_DEPTH: usize = 6;

/// How long a recorded-size plan spends looking for build directories in
/// retained worktrees before it stops and reports how many it did not scan.
/// `doctor`, `certify` and the verify loop take that path, and on a
/// repository with hundreds of retained worktrees an unbounded walk ran for
/// minutes (#460). A measuring `gc plan` scans everything.
pub const HEALTH_CHECK_ARTIFACT_SCAN_BUDGET: std::time::Duration =
    std::time::Duration::from_secs(10);

/// An ignored directory must be larger than this before `gc plan` reports it
/// as an unclassified artifact. Small ignored directories are common project
/// metadata and would make the evidence list noisy without helping an
/// operator find meaningful disk pressure.
pub const UNCLASSIFIED_ARTIFACT_REPORT_THRESHOLD_BYTES: u64 = 4 * 1024;

/// `meta` key holding the last autonomous artifact sweep time.
const ARTIFACT_SWEEP_STAMP_KEY: &str = "gc.artifact_sweep.last_run_ms";
/// Session id the last unfinished sweep stopped after.
///
/// Without it an interrupted pass restarts at the lowest session id, so a
/// budget smaller than one full scan services the head of the list forever and
/// never reaches the backlog behind it. The cadence stamp records *when* a pass
/// ran; this records *how far* it got (#222).
const ARTIFACT_SWEEP_CURSOR_KEY: &str = "gc.artifact_sweep.cursor_session_id";
/// When an unfinished pass last made progress on a broker open.
///
/// An unfinished pass withholds the cadence stamp so the backlog is not hidden
/// for a whole interval. Without this second stamp that made *every* broker
/// open resume the backlog: a repository whose backlog outlasts one budget
/// paid a full budget -- a minute under disk pressure -- on every `gh`, `note`
/// and hook call. Inline continuation is spaced by this instead.
const ARTIFACT_SWEEP_PROGRESS_KEY: &str = "gc.artifact_sweep.last_partial_ms";

/// The build cache an unfinished pass was part-way through removing.
///
/// Unlinking a tree's contents updates its modification time, so without
/// this the next pass mistook its own partial removal for a writer and
/// protected an idle open session's half-removed cache for another idle
/// window. A cache already judged idle and partly removed is finished.
const ARTIFACT_SWEEP_INTERRUPTED_KEY: &str = "gc.artifact_sweep.interrupted_path";
/// Durable request for the detached, cache-only continuation worker.
const ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY: &str = "gc.artifact_sweep.recovery_requested_ms";
/// Highest recovery request generation completed by the cache-only worker.
const ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY: &str = "gc.artifact_sweep.recovery_completed_ms";
/// Retry time for an automatic continuation that made no filesystem progress.
const ARTIFACT_SWEEP_RETRY_AFTER_KEY: &str = "gc.artifact_sweep.retry_after_ms";
/// Bounded last-worker result; overwritten each run rather than appended.
const ARTIFACT_SWEEP_WORKER_STATUS_KEY: &str = "gc.artifact_sweep.worker_status";
/// Internal marker which keeps Broker::open from starting a second inline GC
/// pass before the continuation worker takes its cache-only path.
pub(crate) const ARTIFACT_SWEEP_WORKER_ENV: &str = "AETHYME_GC_ARTIFACT_SWEEP_WORKER";
const ARTIFACT_SWEEP_WORKER_NO_PROGRESS_PASSES: usize = 3;
const ARTIFACT_SWEEP_WORKER_RETRY_MS: i64 = 10 * 60_000;

/// Most a broker open may spend on the artifact sweep, whatever the policy
/// budget or disk pressure. Broker open runs before every command, including
/// the agent hook on every tool call; a larger or pressure-scaled pass belongs
/// to commands that are about reclaiming disk (`gc sweep`, `finish`).
pub const INLINE_ARTIFACT_SWEEP_BUDGET_MS: u64 = 250;

/// Minimum spacing between inline continuations of an unfinished sweep,
/// before the pressure divisor (so hourly-cadence pressure resumes about every
/// 25 seconds rather than on every command).
const INLINE_PARTIAL_PASS_SPACING_MS: i64 = 10 * 60_000;

/// Which caller is sweeping, which decides the budget and what may skip it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactSweepScope {
    /// Broker open: a small fixed budget that disk pressure never scales,
    /// respecting both the cadence and the partial-pass spacing.
    Inline,
    /// An explicit reclaim command: the policy budget scaled by disk
    /// pressure, ignoring cadence and spacing.
    Drain,
    /// Detached recovery: the policy budget scaled by disk pressure, ignoring
    /// cadence and spacing, and without whole-checkout cleanup.
    AutomaticContinuation,
    /// The targeted pass over one just-closed session.
    ClosedSession(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ArtifactSweepOutcome {
    pub directories_reclaimed: usize,
    pub complete: bool,
    /// The checkout auto-cleanup pass that ran in this sweep (#588), if any.
    pub auto_cleanup: Option<crate::AutoCleanupReport>,
}

/// Result of an explicit `gc sweep`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ArtifactSweepReport {
    pub directories_reclaimed: usize,
    pub complete: bool,
    pub budget_ms: u64,
    pub disk_pressured: bool,
    /// Disposable checkouts removed or kept by this sweep (#588).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_cleanup: Option<crate::AutoCleanupReport>,
}

/// Visit order for one sweep pass: everything after the cursor, then the head
/// of the list.
///
/// The wrap matters as much as the resume. Without it a pass starting near the
/// end of the list would report itself complete after a handful of sessions,
/// clear the cursor, and send the next pass back to the top -- so the middle of
/// the list would never be reached. With it, one uninterrupted pass is a full
/// lap whatever the cursor was.
fn sweep_order<T: Clone>(items: &[T], cursor: i64, id: impl Fn(&T) -> i64) -> Vec<T> {
    items
        .iter()
        .filter(|item| id(item) > cursor)
        .chain(items.iter().filter(|item| id(item) <= cursor))
        .cloned()
        .collect()
}

/// Whether a remembered paused tree may be resumed by path inside `root`.
///
/// The path comes from the broker database, not from a scan, so it is
/// re-proved here before anything is removed, each check on its own:
/// - every component below `root` is a plain name -- no parent or root components --
///   because [`Path::starts_with`] is lexical and `<root>/../elsewhere`
///   passes it;
/// - no component from `root` down is a symlink, so the walk cannot be
///   redirected, and the canonical path stays strictly under the canonical
///   root;
/// - its name is still one the unattended lane may remove. The witness may be
///   gone after a partial removal; the name may not change.
fn resumable_interrupted_tree(root: &Path, path: &Path, extras: &[String]) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty()
        || !components
            .iter()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if crate::artifact_catalog::unattended_witness(name, extras).is_none() {
        return false;
    }
    let real_directory = |candidate: &Path| {
        std::fs::symlink_metadata(candidate).is_ok_and(|metadata| metadata.file_type().is_dir())
    };
    if !real_directory(root) {
        return false;
    }
    let mut walked = root.to_path_buf();
    for component in &components {
        walked.push(component);
        if !real_directory(&walked) {
            return false;
        }
    }
    match (std::fs::canonicalize(root), std::fs::canonicalize(path)) {
        (Ok(canonical_root), Ok(canonical_path)) => {
            canonical_path != canonical_root && canonical_path.starts_with(&canonical_root)
        }
        _ => false,
    }
}

/// Wall-clock budget for one sweep pass. A broker open gets the smaller of the
/// policy budget and [`INLINE_ARTIFACT_SWEEP_BUDGET_MS`], never scaled by disk
/// pressure: under pressure the scale used to turn every command into a
/// minute-long sweep. Explicit reclaim gets the policy budget, scaled.
fn artifact_sweep_budget_ms(
    policy_budget_ms: u64,
    scope: ArtifactSweepScope,
    urgency: crate::disk_headroom::SweepUrgency,
) -> u64 {
    match scope {
        ArtifactSweepScope::Inline => policy_budget_ms.min(INLINE_ARTIFACT_SWEEP_BUDGET_MS),
        ArtifactSweepScope::Drain
        | ArtifactSweepScope::AutomaticContinuation
        | ArtifactSweepScope::ClosedSession(_) => {
            policy_budget_ms.saturating_mul(urgency.budget_scale())
        }
    }
}

fn scope_runs_checkout_cleanup(scope: ArtifactSweepScope) -> bool {
    scope != ArtifactSweepScope::AutomaticContinuation
        && !matches!(scope, ArtifactSweepScope::ClosedSession(_))
}

fn is_real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// Whether `path` or any entry directly inside it was modified within
/// `window_ms` of `now`. One `read_dir`: tools that keep writing into a build
/// directory touch its top level (Cargo's `debug/`, Vite's
/// `node_modules/.vite`). An unreadable timestamp counts as recent.
fn modified_within(path: &Path, now: i64, window_ms: i64) -> bool {
    let recent = |metadata: std::io::Result<std::fs::Metadata>| {
        metadata
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .is_none_or(|since| now.saturating_sub(since.as_millis() as i64) < window_ms)
    };
    if recent(std::fs::symlink_metadata(path)) {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return true;
    };
    entries
        .flatten()
        .any(|entry| recent(std::fs::symlink_metadata(entry.path())))
}

fn days_between(now: i64, earlier: i64) -> u32 {
    u32::try_from(now.saturating_sub(earlier) / 86_400_000).unwrap_or(u32::MAX)
}

/// Classify a build directory found beneath `root`, if it is one.
#[cfg(test)]
fn artifact_witness_for(path: &Path) -> Option<ArtifactWitness> {
    artifact_witness_for_with_extras(path, &[])
}

/// Classify a directory the unattended lane may remove, if it is one. Names
/// the shared catalog reserves for reviewed paths never classify here.
fn artifact_witness_for_with_extras(path: &Path, extras: &[String]) -> Option<ArtifactWitness> {
    let name = path.file_name()?.to_str()?;
    let witness = crate::artifact_catalog::unattended_witness(name, extras)?;
    witness.confirms(path).then_some(witness)
}

/// Worktrees whose build caches a plan must not propose.
#[derive(Clone, Copy)]
struct ArtifactExclusions<'a> {
    /// Sessions already scheduled for whole-worktree removal, so the two
    /// candidate sets never double-count the same bytes.
    already_removed: &'a [i64],
    /// Checkouts live sessions work in (see [`overlaps_live_worktree`]).
    live_worktrees: &'a [PathBuf],
}

/// Whether a live session works in `root` or somewhere beneath it.
///
/// Compared on canonical paths where they resolve, because a session row
/// records the path as it was given and the same directory can be spelled
/// through a symlink (`/var` and `/private/var` on macOS). A root *inside* a
/// live checkout is deliberately not an overlap: a session adopted at the main
/// checkout would otherwise shelter every repo-local worktree beneath it, and
/// removing another checkout's ignored build output touches none of its files.
fn overlaps_live_worktree(root: &Path, live_worktrees: &[PathBuf]) -> bool {
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root = canonical(root);
    live_worktrees
        .iter()
        .any(|live| canonical(live).starts_with(&root))
}

fn is_known_artifact_name(name: &str, extras: &[String]) -> bool {
    crate::artifact_catalog::unattended_witness(name, extras).is_some()
}

/// What [`Broker::artifact_candidates`] found, and how many worktrees it ran
/// out of budget to scan.
struct ArtifactCandidates {
    candidates: Vec<GcArtifactCandidate>,
    declined: Vec<GcDeclinedArtifact>,
    not_scanned: usize,
}

#[derive(Default)]
struct ArtifactScan {
    classified: Vec<PathBuf>,
    declined: Vec<PathBuf>,
}

/// Collect classified artifact directories and, when a checkout is supplied,
/// large ignored directories outside the catalog. The latter are opaque
/// evidence only; stopping at their boundary keeps the scan bounded and avoids
/// double-counting nested ignored trees.
fn collect_artifact_dirs_with_report(
    root: &Path,
    current: &Path,
    depth: usize,
    extras: &[String],
    checkout: Option<&GitRepo>,
    scan: &mut ArtifactScan,
) {
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
        if artifact_witness_for_with_extras(&path, extras).is_some() {
            scan.classified.push(path);
            continue;
        }
        let name = path.file_name().and_then(|name| name.to_str());
        if checkout.is_some()
            && name.is_some_and(|name| !is_known_artifact_name(name, extras))
            && repo_relative(root, &path).is_some_and(|relative| {
                checkout.is_some_and(|checkout| checkout.path_is_ignored(&relative))
            })
        {
            scan.declined.push(path);
            continue;
        }
        collect_artifact_dirs_with_report(root, &path, depth + 1, extras, checkout, scan);
    }
}

/// Collect build directories beneath `root`, never descending into one that
/// already matched and never following symlinks. This compatibility wrapper is
/// also used by the unit tests for the built-in catalog.
#[cfg(test)]
fn collect_artifact_dirs(root: &Path, current: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let mut scan = ArtifactScan::default();
    collect_artifact_dirs_with_report(root, current, depth, &[], None, &mut scan);
    found.extend(scan.classified);
}

fn collect_artifact_dirs_with_extras(
    root: &Path,
    current: &Path,
    depth: usize,
    extras: &[String],
    found: &mut Vec<PathBuf>,
) {
    let mut scan = ArtifactScan::default();
    collect_artifact_dirs_with_report(root, current, depth, extras, None, &mut scan);
    found.extend(scan.classified);
}

fn collect_artifact_scan(root: &Path, extras: &[String], checkout: &GitRepo) -> ArtifactScan {
    let mut scan = ArtifactScan::default();
    collect_artifact_dirs_with_report(root, root, 0, extras, Some(checkout), &mut scan);
    scan
}

fn declined_artifacts(
    session_id: i64,
    worktree_path: &str,
    root: &Path,
    paths: Vec<PathBuf>,
    extras: &[String],
) -> Vec<GcDeclinedArtifact> {
    paths
        .into_iter()
        .filter_map(|path| {
            let relative_dir = repo_relative(root, &path)?;
            let usage =
                crate::disk_headroom::directory_usage_without_following_links(&path).ok()?;
            (usage.bytes > UNCLASSIFIED_ARTIFACT_REPORT_THRESHOLD_BYTES).then(|| {
                GcDeclinedArtifact {
                    session_id,
                    worktree_path: worktree_path.to_owned(),
                    relative_dir,
                    estimated_bytes: usage.bytes,
                    estimated_inodes: Some(usage.inodes),
                    reason: declined_reason(&path, extras).into(),
                }
            })
        })
        .collect()
}

/// Why an ignored directory was reported rather than proposed.
fn declined_reason(path: &Path, extras: &[String]) -> &'static str {
    let reviewed_only = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| crate::artifact_catalog::is_reviewed_only(name, extras));
    if reviewed_only {
        "build output name with no witness GC can check; review it with `aethyme broker gc reclaim plan`"
    } else {
        "git-ignored directory is outside the regenerable artifact catalog"
    }
}

/// Whether `archive` is a removal candidate now, and why: `(landed, reason)`.
/// Shared by planning and apply so the rule is re-proved, not remembered.
fn recovery_archive_disposition(
    broker: &Broker,
    archive: &crate::cleanup_resolve::OwnedRecoveryArchive,
    policy: &RetentionPolicy,
    now: i64,
) -> Option<(bool, String)> {
    if policy.recovery_archive_days == 0 {
        return None;
    }
    if broker.recovery_archive_landed(archive) {
        return Some((
            true,
            "its commits have landed and it holds no uncommitted changes; nothing in it is unique"
                .into(),
        ));
    }
    (days_between(now, archive.created_at_ms) >= policy.recovery_archive_days).then(|| {
        (
            false,
            format!(
                "older than recovery_archive_days ({}); contains unlanded work",
                policy.recovery_archive_days
            ),
        )
    })
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

fn sum_inode_estimates(values: impl IntoIterator<Item = Option<u64>>) -> Option<u64> {
    values.into_iter().try_fold(0_u64, |total, estimate| {
        estimate.map(|inodes| total.saturating_add(inodes))
    })
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
        if timestamp.is_none_or(|timestamp| timestamp >= cutoff) {
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
    #[serde(default)]
    remaining_gate_caches: Vec<GcGateCacheCandidate>,
    #[serde(default)]
    remaining_checkpoint_pin_releases: Vec<GcCheckpointPinRelease>,
    #[serde(default)]
    remaining_publication_exposure_expiries: Vec<GcPublicationExposureExpiry>,
    #[serde(default)]
    remaining_recovery_archives: Vec<GcRecoveryArchiveCandidate>,
    rows_removed: usize,
    files_completed: Vec<String>,
    sessions_cleaned: Vec<i64>,
    #[serde(default)]
    artifacts_reclaimed: Vec<String>,
    #[serde(default)]
    orphans_removed: Vec<String>,
    #[serde(default)]
    gate_caches_reclaimed: Vec<String>,
    #[serde(default)]
    checkpoint_pins_released: Vec<i64>,
    #[serde(default)]
    publication_exposures_expired: Vec<i64>,
    #[serde(default)]
    recovery_archives_removed: Vec<String>,
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
            remaining_gate_caches: plan.gate_caches,
            remaining_checkpoint_pin_releases: plan.checkpoint_pin_releases,
            remaining_publication_exposure_expiries: plan.publication_exposure_expiries,
            remaining_recovery_archives: plan.recovery_archives,
            rows_removed: 0,
            files_completed: Vec::new(),
            sessions_cleaned: Vec::new(),
            artifacts_reclaimed: Vec::new(),
            orphans_removed: Vec::new(),
            gate_caches_reclaimed: Vec::new(),
            checkpoint_pins_released: Vec::new(),
            publication_exposures_expired: Vec::new(),
            recovery_archives_removed: Vec::new(),
            reclaimed_bytes: 0,
        }
    }
}

pub(crate) struct GcLock {
    path: PathBuf,
}

impl GcLock {
    pub(crate) fn acquire(main_root: &Path) -> Result<Self, BrokerOpError> {
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

const ARTIFACT_SWEEP_WORKER_LOCK_RELPATH: &str = ".aethyme/run/gc-artifact-worker.lock";

/// Create the dedicated broker runtime directory and prove it remains inside
/// the repository before opening the worker's advisory singleton lock.
fn artifact_sweep_worker_lock_path(main_root: &Path) -> Result<PathBuf, BrokerOpError> {
    let root = std::fs::canonicalize(main_root).map_err(|source| crate::BrokerError::Io {
        path: main_root.to_path_buf(),
        source,
    })?;
    let state_dir = root.join(".aethyme");
    let state_metadata =
        std::fs::symlink_metadata(&state_dir).map_err(|source| crate::BrokerError::Io {
            path: state_dir.clone(),
            source,
        })?;
    if !state_metadata.file_type().is_dir() {
        return Err(worker_path_error(
            &state_dir,
            "repository state path is not a real directory",
        ));
    }

    let run_dir = state_dir.join("run");
    match std::fs::symlink_metadata(&run_dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(worker_path_error(
                &run_dir,
                "broker runtime path is not a real directory",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(&run_dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(crate::BrokerError::Io {
                        path: run_dir.clone(),
                        source,
                    }
                    .into());
                }
            }
            let metadata =
                std::fs::symlink_metadata(&run_dir).map_err(|source| crate::BrokerError::Io {
                    path: run_dir.clone(),
                    source,
                })?;
            if !metadata.file_type().is_dir() {
                return Err(worker_path_error(
                    &run_dir,
                    "broker runtime path is not a real directory",
                ));
            }
        }
        Err(source) => {
            return Err(crate::BrokerError::Io {
                path: run_dir.clone(),
                source,
            }
            .into());
        }
    }
    let canonical_run =
        std::fs::canonicalize(&run_dir).map_err(|source| crate::BrokerError::Io {
            path: run_dir.clone(),
            source,
        })?;
    if !canonical_run.starts_with(&root) {
        return Err(worker_path_error(
            &run_dir,
            "broker runtime path resolves outside the repository",
        ));
    }

    let lock_path = canonical_run.join("gc-artifact-worker.lock");
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(worker_path_error(
                &lock_path,
                "worker lock path is not a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(crate::BrokerError::Io {
                path: lock_path,
                source,
            }
            .into());
        }
    }
    Ok(lock_path)
}

fn worker_path_error(path: &Path, message: &str) -> BrokerOpError {
    crate::BrokerError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, message.to_string()),
    }
    .into()
}

fn artifact_worker_process_is_suppressed() -> bool {
    cfg!(test)
        || std::env::var_os(ARTIFACT_SWEEP_WORKER_ENV).is_some()
        || [
            crate::BROKER_DB_ENV,
            "AETHYME_TEST_DB_SUFFIX",
            crate::disk_headroom::TEST_AVAILABLE_BYTES_ENV,
            crate::disk_headroom::TEST_AVAILABLE_INODES_ENV,
            "CARGO_MANIFEST_DIR",
            "CARGO_TARGET_DIR",
            "CARGO_BIN_EXE_aethyme",
        ]
        .iter()
        .any(|key| std::env::var_os(key).is_some())
}

/// Resolve only the router and paired engine that belong to this running
/// installation. Never fall back to PATH: the detached process must be the
/// exact broker runtime that recorded the request.
fn artifact_worker_binaries() -> Option<(PathBuf, PathBuf)> {
    let executable = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    let installation = crate::update::detect_installation(&executable);
    if installation.method == crate::update::InstallationMethod::Unknown {
        return None;
    }
    if installation.method == crate::update::InstallationMethod::ManualArchive
        && executable.components().any(|component| {
            matches!(component, Component::Normal(name) if name == "target" || name == "deps")
        })
    {
        return None;
    }
    let router = std::fs::canonicalize(installation.router_path).ok()?;
    let engine = std::fs::canonicalize(installation.engine_path?).ok()?;
    if router != executable
        || router.parent().is_none()
        || router.parent() != engine.parent()
        || !engine.is_file()
    {
        return None;
    }
    Some((router, engine))
}

fn worker_binary_version(binary: &Path) -> Option<String> {
    let mut child = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut stdout = Vec::new();
    child
        .stdout
        .take()?
        .take(4096)
        .read_to_end(&mut stdout)
        .ok()?;
    String::from_utf8(stdout)
        .ok()?
        .lines()
        .next()
        .map(str::to_string)
}

pub(crate) fn artifact_worker_runtime_is_aligned() -> bool {
    let Some((router, engine)) = artifact_worker_binaries() else {
        return false;
    };
    let router_identity = worker_binary_version(&router)
        .as_deref()
        .and_then(crate::install_health::build_identity);
    let engine_identity = worker_binary_version(&engine)
        .as_deref()
        .and_then(crate::install_health::build_identity);
    matches!(
        crate::install_health::pair_state(router_identity, engine_identity),
        crate::install_health::PairState::Aligned(_)
    )
}

fn worker_retry_pending(store: &crate::BrokerStore, now: i64) -> Result<bool, BrokerOpError> {
    Ok(store
        .meta_get(ARTIFACT_SWEEP_RETRY_AFTER_KEY)?
        .and_then(|value| value.parse::<i64>().ok())
        .is_some_and(|retry_after| retry_after > now))
}

fn artifact_sweep_worker_lock_file(main_root: &Path) -> Result<std::fs::File, BrokerOpError> {
    let path = artifact_sweep_worker_lock_path(main_root)?;
    let file =
        crate::file_lock::open_lock_file(&path).map_err(|source| crate::BrokerError::Io {
            path: path.clone(),
            source,
        })?;
    let named = std::fs::symlink_metadata(&path).map_err(|source| crate::BrokerError::Io {
        path: path.clone(),
        source,
    })?;
    let opened = file.metadata().map_err(|source| crate::BrokerError::Io {
        path: path.clone(),
        source,
    })?;
    if !named.file_type().is_file() || !opened.file_type().is_file() {
        return Err(worker_path_error(
            &path,
            "worker lock path changed while it was opened",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if named.dev() != opened.dev() || named.ino() != opened.ino() {
            return Err(worker_path_error(
                &path,
                "worker lock path changed while it was opened",
            ));
        }
    }
    Ok(file)
}

/// Try to become the one process allowed to schedule or run continuation.
pub(crate) fn try_artifact_sweep_worker_lock(
    main_root: &Path,
) -> Result<Option<crate::file_lock::ExclusiveFileLock>, BrokerOpError> {
    let file = artifact_sweep_worker_lock_file(main_root)?;
    crate::file_lock::ExclusiveFileLock::try_acquire(file).map_err(|source| {
        crate::BrokerError::Io {
            path: main_root.join(ARTIFACT_SWEEP_WORKER_LOCK_RELPATH),
            source,
        }
        .into()
    })
}

/// Worker entry acquires the same lock before Broker::open, so its open-time
/// maintenance cannot overlap another worker or create an ordinary inline
/// sweep ahead of cache-only continuation.
pub(crate) fn acquire_artifact_sweep_worker_lock(
    main_root: &Path,
) -> Result<crate::file_lock::ExclusiveFileLock, BrokerOpError> {
    let file = artifact_sweep_worker_lock_file(main_root)?;
    crate::file_lock::ExclusiveFileLock::acquire(file).map_err(|source| {
        crate::BrokerError::Io {
            path: main_root.join(ARTIFACT_SWEEP_WORKER_LOCK_RELPATH),
            source,
        }
        .into()
    })
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
    let result = crate::atomic_file::with_synced_temporary(path, bytes, |temporary| {
        std::fs::rename(temporary, path)
    });
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
pub(crate) enum TreeRemoval {
    /// The directory is gone.
    Complete,
    /// The budget ran out. The directory is still there, still carrying the
    /// evidence that classifies it, and the next pass resumes it.
    Interrupted,
}

#[derive(Debug)]
pub(crate) struct TreeRemovalError {
    pub(crate) source: std::io::Error,
    pub(crate) entries_reclaimed: usize,
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
pub(crate) fn remove_condemned_tree(
    dir: &Path,
    keep_until_last: Option<&str>,
    deadline: Option<Instant>,
) -> std::io::Result<TreeRemoval> {
    remove_condemned_tree_with_progress(dir, keep_until_last, deadline)
        .map(|(outcome, _)| outcome)
        .map_err(|error| error.source)
}

/// Remove a condemned tree and count entries actually unlinked. A partial
/// deletion can keep the same cursor and interrupted path across passes, so
/// the continuation worker uses this count instead of mistaking that stable
/// path for a stalled sweep.
pub(crate) fn remove_condemned_tree_with_progress(
    dir: &Path,
    keep_until_last: Option<&str>,
    deadline: Option<Instant>,
) -> Result<(TreeRemoval, usize), TreeRemovalError> {
    let mut entries_reclaimed = 0;
    let mut attempts_left = TREE_REMOVAL_ATTEMPTS;
    loop {
        attempts_left -= 1;
        if !is_real_directory(dir) {
            return Ok((TreeRemoval::Complete, entries_reclaimed));
        }
        match drain_directory(dir, keep_until_last, deadline, &mut entries_reclaimed) {
            Ok(TreeRemoval::Interrupted) => {
                return Ok((TreeRemoval::Interrupted, entries_reclaimed));
            }
            Ok(TreeRemoval::Complete) => {}
            Err(source) => {
                return Err(TreeRemovalError {
                    source,
                    entries_reclaimed,
                });
            }
        }
        if let Some(name) = keep_until_last {
            if let Err(source) =
                remove_entry_with_progress(&dir.join(name), false, &mut entries_reclaimed)
            {
                return Err(TreeRemovalError {
                    source,
                    entries_reclaimed,
                });
            }
        }
        match std::fs::remove_dir(dir) {
            Ok(()) => {
                entries_reclaimed = entries_reclaimed.saturating_add(1);
                return Ok((TreeRemoval::Complete, entries_reclaimed));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((TreeRemoval::Complete, entries_reclaimed));
            }
            Err(source) if attempts_left == 0 => {
                return Err(TreeRemovalError {
                    source,
                    entries_reclaimed,
                });
            }
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
    entries_reclaimed: &mut usize,
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
        if is_directory
            && drain_directory(&path, None, deadline, entries_reclaimed)?
                == TreeRemoval::Interrupted
        {
            return Ok(TreeRemoval::Interrupted);
        }
        remove_entry_with_progress(&path, is_directory, entries_reclaimed)?;
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
    remove_entry_with_progress(path, is_directory, &mut 0)
}

fn remove_entry_with_progress(
    path: &Path,
    is_directory: bool,
    entries_reclaimed: &mut usize,
) -> std::io::Result<()> {
    let removed = if is_directory {
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Ok(()) => {
            *entries_reclaimed = entries_reclaimed.saturating_add(1);
            Ok(())
        }
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
        excluded: ArtifactExclusions<'_>,
        size_scan: crate::SizeScan,
    ) -> ArtifactCandidates {
        let budget = (!size_scan.measures()).then_some(HEALTH_CHECK_ARTIFACT_SCAN_BUDGET);
        self.artifact_candidates_within(
            evaluated_at,
            policy,
            cleanup,
            sessions,
            excluded,
            size_scan,
            budget,
        )
    }

    /// [`Self::artifact_candidates`] with an explicit scan budget, `None` for
    /// a complete scan. Worktrees left once the budget is spent are counted,
    /// not scanned.
    #[allow(clippy::too_many_arguments)]
    fn artifact_candidates_within(
        &self,
        evaluated_at: i64,
        policy: &RetentionPolicy,
        cleanup: &[crate::CleanupWorktreePlan],
        sessions: &BTreeMap<i64, crate::Session>,
        excluded: ArtifactExclusions<'_>,
        size_scan: crate::SizeScan,
        budget: Option<std::time::Duration>,
    ) -> ArtifactCandidates {
        let ArtifactExclusions {
            already_removed,
            live_worktrees,
        } = excluded;
        let started = std::time::Instant::now();
        let mut candidates = Vec::new();
        let mut declined = Vec::new();
        let mut not_scanned = 0;
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
            // A finished session's directory can be adopted again by a new
            // session, which leaves the old row pointing at a live checkout.
            // Its build output is then the live session's, and a cargo build
            // may be writing into it right now.
            if overlaps_live_worktree(&root, live_worktrees) {
                continue;
            }
            if budget.is_some_and(|budget| started.elapsed() >= budget) {
                not_scanned += 1;
                continue;
            }
            let Ok(checkout) = GitRepo::discover(&root) else {
                continue;
            };
            // Ignored directories outside the catalog are only reported with
            // their size, so a pass that measures nothing skips finding them:
            // that search costs one `git check-ignore` per directory walked.
            let artifact_scan = if size_scan.measures() {
                collect_artifact_scan(&root, &policy.artefact_directories, &checkout)
            } else {
                let mut classified = Vec::new();
                collect_artifact_dirs_with_extras(
                    &root,
                    &root,
                    0,
                    &policy.artefact_directories,
                    &mut classified,
                );
                ArtifactScan {
                    classified,
                    declined: Vec::new(),
                }
            };
            for dir in artifact_scan.classified {
                let Some(relative) = repo_relative(&root, &dir) else {
                    continue;
                };
                if !checkout.path_is_ignored(&relative) {
                    continue;
                }
                // Discovery is `read_dir` plus one `check-ignore`; sizing is
                // the walk. A recorded-size pass keeps the candidate and
                // reports zero bytes with an unknown inode count -- dropping
                // it would understate the count (#176). A measured directory
                // holding no bytes and nothing but itself (an empty `target/`)
                // frees nothing worth a candidate.
                let usage = if size_scan.measures() {
                    match crate::disk_headroom::directory_usage_without_following_links(&dir) {
                        Ok(usage) if usage.bytes == 0 && usage.inodes <= 1 => continue,
                        Ok(usage) => Some(usage),
                        Err(_) => continue,
                    }
                } else {
                    None
                };
                candidates.push(GcArtifactCandidate {
                    session_id: item.session_id,
                    worktree_path: item.worktree_path.clone(),
                    relative_dir: relative,
                    estimated_bytes: usage.map_or(0, |usage| usage.bytes),
                    estimated_inodes: usage.map(|usage| usage.inodes),
                    idle_days,
                });
            }
            if size_scan.measures() {
                declined.extend(declined_artifacts(
                    item.session_id,
                    &item.worktree_path,
                    &root,
                    artifact_scan.declined,
                    &policy.artefact_directories,
                ));
            }
        }
        candidates.sort_by(|left, right| {
            (left.session_id, &left.relative_dir).cmp(&(right.session_id, &right.relative_dir))
        });
        declined.sort_by(|left, right| {
            (left.session_id, &left.relative_dir).cmp(&(right.session_id, &right.relative_dir))
        });
        ArtifactCandidates {
            candidates,
            declined,
            not_scanned,
        }
    }

    /// Recovery archives `gc plan` may propose, plus the inventory of every
    /// archive under this repository's archive directory.
    ///
    /// Never autonomous: an archive exists because its work was not proved to
    /// have landed, so each one is only ever a reviewed candidate. It becomes
    /// one when its commits have since landed and it holds no uncommitted
    /// changes (nothing in it is unique), or once it is older than
    /// `recovery_archive_days`. `0` keeps every archive and proposes none.
    fn recovery_archive_candidates(
        &self,
        evaluated_at: i64,
        policy: &RetentionPolicy,
        scan: crate::SizeScan,
        deadline: Option<std::time::Instant>,
    ) -> Result<
        (
            Vec<GcRecoveryArchiveCandidate>,
            Option<RecoveryArchiveInventory>,
            bool,
        ),
        BrokerOpError,
    > {
        // Resolving the archive location depends on the repository worktree
        // key. Once this report's budget is exhausted, do not start that Git
        // lookup merely to return an inventory the caller must defer.
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Ok((Vec::new(), None, false));
        }
        let crate::cleanup_resolve::RecoveryArchiveScan {
            root,
            owned,
            unowned,
        } = self.recovery_archives()?;
        if owned.is_empty() && unowned.is_empty() {
            return Ok((Vec::new(), None, true));
        }
        let size = |path: &Path| {
            if scan.measures() {
                directory_size_without_following_links(path).unwrap_or(0)
            } else {
                0
            }
        };
        let mut inventory = RecoveryArchiveInventory {
            root: root.to_string_lossy().into_owned(),
            count: owned.len() + unowned.len(),
            ..RecoveryArchiveInventory::default()
        };
        let mut candidates = Vec::new();
        let mut complete = true;
        for archive in &owned {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                complete = false;
                break;
            }
            let bytes = size(&archive.path);
            inventory.estimated_bytes = inventory.estimated_bytes.saturating_add(bytes);
            inventory.oldest_created_at_ms = Some(
                inventory
                    .oldest_created_at_ms
                    .map_or(archive.created_at_ms, |oldest| {
                        oldest.min(archive.created_at_ms)
                    }),
            );
            if let Some((landed, reason)) =
                recovery_archive_disposition(self, archive, policy, evaluated_at)
            {
                candidates.push(GcRecoveryArchiveCandidate {
                    path: archive.path.to_string_lossy().into_owned(),
                    session_id: archive.session_id,
                    head: archive.head.clone(),
                    created_at_ms: archive.created_at_ms,
                    landed,
                    estimated_bytes: bytes,
                    reason,
                });
            }
        }
        for (path, reason) in &unowned {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                complete = false;
                break;
            }
            inventory.estimated_bytes = inventory.estimated_bytes.saturating_add(size(path));
            inventory
                .unowned
                .push(format!("{}: {reason}", path.to_string_lossy()));
        }
        candidates.sort_by(|left, right| left.path.cmp(&right.path));
        Ok((candidates, Some(inventory), complete))
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
        scan: crate::SizeScan,
        deadline: Option<std::time::Instant>,
    ) -> Result<(Vec<GcOrphanCandidate>, bool), BrokerOpError> {
        // Resolving the repository worktree key shells out to Git. A doctor
        // pass whose inspection budget is already spent must name this
        // inventory as deferred without starting another subprocess.
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Ok((Vec::new(), false));
        }
        let plan = self.worktree_root_plan()?;
        let Some(container) = plan.root_container.clone() else {
            return Ok((Vec::new(), true));
        };
        let Ok(entries) = std::fs::read_dir(&container) else {
            return Ok((Vec::new(), false));
        };
        let mut candidates = Vec::new();
        let mut complete = true;
        for entry in entries.flatten() {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                complete = false;
                break;
            }
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
            let usage = if scan.measures() {
                if let Some(deadline) = deadline {
                    crate::disk_headroom::directory_usage_bounded(&root, deadline)
                } else {
                    crate::disk_headroom::directory_usage_without_following_links(&root).ok()
                }
            } else {
                None
            };
            if scan.measures() && usage.is_none() {
                complete = false;
                break;
            }
            candidates.push(GcOrphanCandidate {
                repository_key: marker.repository_key,
                worktree_root: root.to_string_lossy().into_owned(),
                repository_root: marker.repository_root.to_string_lossy().into_owned(),
                estimated_bytes: usage.map_or(0, |usage| usage.bytes),
                estimated_inodes: usage.map(|usage| usage.inodes),
                reason: "owning repository no longer exists".into(),
            });
        }
        candidates.sort_by(|left, right| left.worktree_root.cmp(&right.worktree_root));
        Ok((candidates, complete))
    }

    /// The full audit: walk every retained worktree, build cache and orphaned
    /// root, and size what it finds.
    ///
    /// This is the expensive path, and the only one whose digest authorizes
    /// `gc apply`. On the machine behind #176 it took over five minutes, which
    /// is acceptable for something an operator asks for and unacceptable for
    /// anything that runs on its own -- hence [`Broker::gc_plan_recorded`].
    pub fn gc_plan(&mut self) -> Result<GcPlan, BrokerOpError> {
        self.gc_plan_scanned(crate::SizeScan::Measure, false)
    }

    /// [`Broker::gc_plan`], also proposing the most recently used entry of
    /// each gate cache kind. That entry is what the next gate reuses, so its
    /// removal is paid back as a cold build of the same size; it is opt-in
    /// (`--include-active-gate-cache`) for when the disk cannot wait (#295).
    pub fn gc_plan_including_active_gate_cache(&mut self) -> Result<GcPlan, BrokerOpError> {
        self.gc_plan_scanned(crate::SizeScan::Measure, true)
    }

    /// The same plan assembled without walking a single tree.
    ///
    /// Everything but sizing survives: row queries, blockers, gate logs, and
    /// build-cache discovery are all cheap. Byte totals come from what the
    /// last full audit recorded, so they are floors, and the plan carries no
    /// digest -- a plan that does not know how big things are must not be able
    /// to authorize removing them.
    pub fn gc_plan_recorded(&mut self) -> Result<GcPlan, BrokerOpError> {
        self.gc_plan_scanned(crate::SizeScan::Recorded, false)
    }

    fn gc_plan_scanned(
        &mut self,
        scan: crate::SizeScan,
        include_active_gate_cache: bool,
    ) -> Result<GcPlan, BrokerOpError> {
        self.gc_plan_scanned_until(scan, include_active_gate_cache, None)
    }

    fn gc_plan_scanned_until(
        &mut self,
        scan: crate::SizeScan,
        include_active_gate_cache: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<GcPlan, BrokerOpError> {
        let evaluated_at = now_ms();
        let main_root = self.main_root().to_path_buf();
        let retention_config = load_retention_policy_report(&main_root)?;
        let policy = retention_config.policy;
        let retention_config_warnings = retention_config.warnings;
        let exposure_cutoff = cutoff(evaluated_at, policy.publication_exposure_days);
        let cleanup = match deadline.filter(|_| !scan.measures()) {
            Some(deadline) => self.cleanup_plan_scanned_within(
                scan,
                Some(deadline.saturating_duration_since(std::time::Instant::now())),
            )?,
            None => self.cleanup_plan_scanned(scan)?,
        };
        let mut deferred_checks = Vec::new();
        if cleanup.eligibility_not_inspected_count > 0 {
            deferred_checks.push("cleanup_eligibility".into());
        }
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
        let checkpoint_pin_releases = self.store().gc_checkpoint_pin_candidates()?;
        let live_sessions = self.store().live_sessions()?;
        let advisories = self.store().advisories(false)?;
        let exposures = self.store().outstanding_entry_path_exposures()?;
        let operations = self.store().coordinated_operations()?;
        let publication_exposure_expiries = exposures
            .iter()
            .filter(|exposure| exposure.created_at < exposure_cutoff)
            .map(|exposure| GcPublicationExposureExpiry {
                exposure_id: exposure.id,
                queue_entry_id: exposure.queue_entry_id,
                created_at: exposure.created_at,
                age_days: days_between(evaluated_at, exposure.created_at),
                reason: format!(
                    "publication exposure exceeded the {} day retention policy; explicit expiry does not claim publication was verified",
                    policy.publication_exposure_days
                ),
            })
            .collect::<Vec<_>>();
        let mut blockers = Vec::new();
        let mut blocked_worktree_bytes = BTreeMap::<(String, Option<i64>), u64>::new();
        for pin in &checkpoint_pin_releases {
            blockers.push(GcBlocker {
                kind: "accepted_checkpoint".into(),
                id: Some(pin.queue_entry_id),
                reason: format!("session {}: {}", pin.session_id, pin.reason),
            });
        }
        for session in sessions.values() {
            if let Some(queue_entry_id) = session.accepted_queue_entry_id {
                if checkpoint_pin_releases
                    .iter()
                    .any(|pin| pin.session_id == session.id && pin.queue_entry_id == queue_entry_id)
                {
                    continue;
                }
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
        for session in &live_sessions {
            blockers.push(GcBlocker {
                kind: "live_session".into(),
                id: Some(session.id),
                reason: "live sessions and their rows are never aged out".into(),
            });
        }
        for advisory in &advisories {
            blockers.push(GcBlocker {
                kind: "outstanding_advisory".into(),
                id: Some(advisory.id),
                reason: "outstanding and acknowledged advisories remain authoritative".into(),
            });
        }
        for exposure in &exposures {
            blockers.push(GcBlocker {
                kind: "publication_exposure".into(),
                id: Some(exposure.id),
                reason: "publication has not been verified".into(),
            });
        }
        for operation in &operations {
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
        let live_worktrees = live_sessions
            .iter()
            .map(|session| PathBuf::from(&session.worktree_path))
            .collect::<Vec<_>>();
        let grace_ms = i64::from(policy.closed_worktree_grace_hours).saturating_mul(3_600_000);
        let mut worktrees = Vec::new();
        for item in cleanup.worktrees.iter().cloned() {
            let Some(session) = sessions.get(&item.session_id) else {
                continue;
            };
            let closed_at = session.closed_at.unwrap_or(session.updated_at);
            let retained_bytes = item.estimated_bytes.unwrap_or(0);
            // A closed session's directory can be adopted again, by `start
            // --replace-stale` or by hand, which leaves this row naming a
            // checkout a live session is working in. Its cleanliness and
            // provenance then describe the live session's work, not this
            // one's, and no proof about a closed row authorizes removing it.
            if item.worktree_present
                && overlaps_live_worktree(Path::new(&item.worktree_path), &live_worktrees)
            {
                let blocker = GcBlocker {
                    kind: "live_worktree".into(),
                    id: Some(item.session_id),
                    reason: "the worktree is the checkout of a live session".into(),
                };
                blocked_worktree_bytes.insert((blocker.kind.clone(), blocker.id), retained_bytes);
                blockers.push(blocker);
                continue;
            }
            // Cleanup eligibility is the representation proof. Once it holds,
            // GC must schedule the same worktree regardless of its age; an
            // age gate here made `cleanup --all-cleaned` and `gc` disagree.
            // The one exception is a short grace period after a state-only
            // close: that close keeps the checkout on purpose, so GC waits
            // before second-guessing it. Explicit `finish cleanup` does not.
            if item.eligible()
                && item.worktree_present
                && session.cleanup_state == crate::SessionCleanupState::Closed
                && session.cleanup_completed_at.is_none()
                && evaluated_at.saturating_sub(closed_at) < grace_ms
            {
                let blocker = GcBlocker {
                    kind: "closed_worktree_grace".into(),
                    id: Some(item.session_id),
                    reason: format!(
                        "closed less than the {} hour grace period ago; eligible afterwards: {}",
                        policy.closed_worktree_grace_hours, item.reason
                    ),
                };
                blocked_worktree_bytes.insert((blocker.kind.clone(), blocker.id), retained_bytes);
                blockers.push(blocker);
                continue;
            }
            if item.eligible() {
                worktrees.push(GcWorktreeCandidate {
                    session_id: item.session_id,
                    worktree_path: item.worktree_path,
                    worktree_present: item.worktree_present,
                    branch_ref: item.branch_ref,
                    branch_tip: item.branch_tip,
                    estimated_bytes: retained_bytes,
                    estimated_inodes: item.estimated_inodes,
                    closed_at,
                });
                continue;
            }
            // Without representation proof, the age policy is the first
            // bounded protection. Once that window has elapsed, retain the
            // more specific provenance blocker instead of ever scheduling
            // the contribution for whole-worktree removal.
            let blocker = if closed_at >= worktree_cutoff {
                GcBlocker {
                    kind: "retention_age".into(),
                    id: Some(item.session_id),
                    reason: format!(
                        "closed worktree is younger than the {} day policy; cleanup eligibility: {}",
                        policy.closed_worktrees_days, item.reason
                    ),
                }
            } else {
                GcBlocker {
                    kind: "unproven_contribution".into(),
                    id: Some(item.session_id),
                    reason: item.reason,
                }
            };
            blocked_worktree_bytes.insert((blocker.kind.clone(), blocker.id), retained_bytes);
            blockers.push(blocker);
        }

        // Adopted checkouts were never created by the broker, so neither
        // cleanup nor GC removes them; `cleanup_plan` skips them entirely.
        // Saying so is the whole of the lane: otherwise a closed adopted
        // checkout is disk nobody's arithmetic mentions.
        let canonical_main =
            std::fs::canonicalize(&main_root).unwrap_or_else(|_| main_root.clone());
        for session in sessions.values() {
            if session.origin != crate::SessionOrigin::Adopted
                || session.cleanup_state != crate::SessionCleanupState::Closed
                || session.cleanup_completed_at.is_some()
            {
                continue;
            }
            let path = Path::new(&session.worktree_path);
            if !is_real_directory(path)
                || std::fs::canonicalize(path).is_ok_and(|path| path == canonical_main)
                || overlaps_live_worktree(path, &live_worktrees)
            {
                continue;
            }
            blockers.push(GcBlocker {
                kind: "adopted_worktree".into(),
                id: Some(session.id),
                reason: format!(
                    "adopted checkout {} was not created by the broker and is never removed by GC",
                    session.worktree_path
                ),
            });
        }

        let removed_sessions = worktrees
            .iter()
            .map(|worktree| worktree.session_id)
            .collect::<Vec<_>>();
        let artifact_budget =
            deadline.map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()));
        let ArtifactCandidates {
            candidates: mut artifacts,
            declined: declined_artifacts,
            not_scanned: artifact_worktrees_not_scanned,
        } = match artifact_budget {
            Some(budget) => self.artifact_candidates_within(
                evaluated_at,
                &policy,
                &cleanup.worktrees,
                &sessions,
                ArtifactExclusions {
                    already_removed: &removed_sessions,
                    live_worktrees: &live_worktrees,
                },
                scan,
                Some(budget),
            ),
            None => self.artifact_candidates(
                evaluated_at,
                &policy,
                &cleanup.worktrees,
                &sessions,
                ArtifactExclusions {
                    already_removed: &removed_sessions,
                    live_worktrees: &live_worktrees,
                },
                scan,
            ),
        };
        if artifact_worktrees_not_scanned > 0 {
            deferred_checks.push("artifact_scan".into());
        }
        let (mut orphans, orphans_complete) =
            self.orphan_candidates(evaluated_at, &policy, &mut blockers, scan, deadline)?;
        if !orphans_complete {
            deferred_checks.push("orphan_scan".into());
        }
        let (recovery_archives, recovery_archive_inventory, archives_complete) =
            self.recovery_archive_candidates(evaluated_at, &policy, scan, deadline)?;
        if !archives_complete {
            deferred_checks.push("recovery_archives".into());
        }
        // Rooted at the per-user cache directory, not at any worktree: that
        // is why no total here ever counted it (#295).
        let mut size_records = crate::measurement::load_size_records(&main_root);
        // A configuration that does not load names no key, which leaves every
        // kind to the most-recent-use fallback.
        let configured_keys = crate::gates::load_gates(&main_root)
            .map(|gates| {
                gates
                    .into_iter()
                    .filter_map(|gate| gate.managed_cache.map(|cache| cache.key))
                    .collect()
            })
            .unwrap_or_default();
        let (gate_cache, gate_caches) = match crate::gate_cache_gc::location(&main_root) {
            Some(location) => {
                let (inventory, candidates) = crate::gate_cache_gc::inspect(
                    &main_root,
                    &location,
                    crate::gate_cache_gc::KeepRule {
                        budget_bytes: policy.gate_cache_bytes_budget,
                        include_active: include_active_gate_cache,
                        configured_keys: &configured_keys,
                    },
                    evaluated_at,
                    scan,
                    &mut size_records,
                );
                (Some(inventory), candidates)
            }
            None => (None, Vec::new()),
        };
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            deferred_checks.push("gate_cache_inventory".into());
        }
        if scan.measures() {
            crate::warn_unrecorded(
                "record gate cache sizes",
                crate::measurement::save_size_records(&main_root, &size_records),
            );
        }

        rows.sort_by_key(|row| (row.kind, row.id));
        let mut files = files.into_values().collect::<Vec<_>>();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        // Ordered below, once the budget is known: which rule applies is a
        // function of how much is retained, so it cannot be decided yet.
        worktrees.sort_by_key(|worktree| worktree.session_id);
        blockers.sort_by(|left, right| {
            (&left.kind, left.id, &left.reason).cmp(&(&right.kind, right.id, &right.reason))
        });
        blockers.dedup();
        let mut blocker_facts = BTreeMap::<(String, Option<i64>), (u64, i64)>::new();
        for pin in &checkpoint_pin_releases {
            blocker_facts.insert(
                ("accepted_checkpoint".into(), Some(pin.queue_entry_id)),
                (pin.estimated_bytes, pin.recorded_at),
            );
        }
        for session in &live_sessions {
            blocker_facts.insert(
                ("live_session".into(), Some(session.id)),
                (0, session.created_at),
            );
        }
        for advisory in &advisories {
            blocker_facts.insert(
                ("outstanding_advisory".into(), Some(advisory.id)),
                (0, advisory.created_at),
            );
        }
        for exposure in &exposures {
            let estimated_bytes = exposure
                .promotion_sha
                .len()
                .saturating_add(exposure.paths.iter().map(String::len).sum::<usize>())
                .saturating_add(96) as u64;
            blocker_facts.insert(
                ("publication_exposure".into(), Some(exposure.id)),
                (estimated_bytes, exposure.created_at),
            );
        }
        for operation in &operations {
            if matches!(
                operation.status,
                OperationStatus::Prepared
                    | OperationStatus::Running
                    | OperationStatus::OutcomeUnknown
            ) {
                blocker_facts.insert(
                    ("unresolved_operation".into(), Some(operation.id)),
                    (0, operation.created_at),
                );
            }
        }
        for ((kind, id), retained_bytes) in &blocked_worktree_bytes {
            if let Some(session_id) = id
                && let Some(session) = sessions.get(session_id)
            {
                blocker_facts.insert(
                    (kind.clone(), *id),
                    (
                        *retained_bytes,
                        session.closed_at.unwrap_or(session.updated_at),
                    ),
                );
            }
        }
        let mut grouped = BTreeMap::<String, (usize, u64, Option<(i64, Option<i64>)>)>::new();
        for blocker in &blockers {
            let (retained_bytes, recorded_at) = blocker_facts
                .get(&(blocker.kind.clone(), blocker.id))
                .copied()
                .unwrap_or((0, 0));
            let entry = grouped.entry(blocker.kind.clone()).or_default();
            entry.0 += 1;
            entry.1 = entry.1.saturating_add(retained_bytes);
            if recorded_at > 0 {
                let member = (recorded_at, blocker.id);
                entry.2 = Some(entry.2.map_or(member, |oldest| {
                    if member.0 < oldest.0 || (member.0 == oldest.0 && member.1 < oldest.1) {
                        member
                    } else {
                        oldest
                    }
                }));
            }
        }
        let mut blocker_summary = grouped
            .into_iter()
            .map(|(kind, (count, retained_bytes, oldest))| {
                let age_policy_days = match kind.as_str() {
                    "accepted_checkpoint" => Some(0),
                    "publication_exposure" => Some(policy.publication_exposure_days),
                    "retention_age" => Some(policy.closed_worktrees_days),
                    _ => None,
                };
                let oldest_id = oldest.and_then(|(_, id)| id);
                let oldest_recorded_at = oldest.map(|(recorded_at, _)| recorded_at);
                let oldest_age_days =
                    oldest_recorded_at.map(|recorded_at| days_between(evaluated_at, recorded_at));
                let age_exceeded = age_policy_days
                    .zip(oldest_age_days)
                    .is_some_and(|(policy_days, age_days)| age_days >= policy_days);
                GcBlockerSummary {
                    kind,
                    count,
                    retained_bytes,
                    oldest_id,
                    oldest_recorded_at,
                    oldest_age_days,
                    age_policy_days,
                    age_exceeded,
                }
            })
            .collect::<Vec<_>>();
        blocker_summary.sort_by(|left, right| {
            right
                .retained_bytes
                .cmp(&left.retained_bytes)
                .then_with(|| right.count.cmp(&left.count))
                .then_with(|| left.kind.cmp(&right.kind))
        });
        let mut worktree_blocker_summary = blocked_worktree_bytes
            .iter()
            .filter_map(|((kind, id), retained_bytes)| {
                let blocker = blockers
                    .iter()
                    .find(|blocker| blocker.kind == *kind && blocker.id == *id)?;
                Some((blocker.kind.clone(), *retained_bytes))
            })
            .fold(
                BTreeMap::<String, (usize, u64)>::new(),
                |mut summary, (kind, bytes)| {
                    let entry = summary.entry(kind).or_default();
                    entry.0 += 1;
                    entry.1 = entry.1.saturating_add(bytes);
                    summary
                },
            )
            .into_iter()
            .map(|(kind, (count, retained_bytes))| GcWorktreeBlockerSummary {
                kind,
                count,
                retained_bytes,
            })
            .collect::<Vec<_>>();
        worktree_blocker_summary.sort_by(|left, right| {
            right
                .retained_bytes
                .cmp(&left.retained_bytes)
                .then_with(|| left.kind.cmp(&right.kind))
        });
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
            .chain(gate_caches.iter().map(|cache| cache.estimated_bytes))
            .fold(0_u64, u64::saturating_add);
        let estimated_build_output_reclaimable_bytes = artifacts
            .iter()
            .map(|artifact| artifact.estimated_bytes)
            .chain(gate_caches.iter().map(|cache| cache.estimated_bytes))
            .fold(0_u64, u64::saturating_add);
        let estimated_declined_artifact_bytes = declined_artifacts
            .iter()
            .map(|artifact| artifact.estimated_bytes)
            .fold(0_u64, u64::saturating_add);
        let estimated_reclaimable_inodes = sum_inode_estimates(
            worktrees
                .iter()
                .map(|worktree| worktree.estimated_inodes)
                .chain(artifacts.iter().map(|artifact| artifact.estimated_inodes))
                .chain(orphans.iter().map(|orphan| orphan.estimated_inodes))
                .chain(gate_caches.iter().map(|cache| cache.estimated_inodes)),
        );
        let estimated_build_output_reclaimable_inodes = sum_inode_estimates(
            artifacts
                .iter()
                .map(|artifact| artifact.estimated_inodes)
                .chain(gate_caches.iter().map(|cache| cache.estimated_inodes)),
        );
        let estimated_declined_artifact_inodes = sum_inode_estimates(
            declined_artifacts
                .iter()
                .map(|artifact| artifact.estimated_inodes),
        );
        // Retained bytes describe disk pressure, not authorized work: every
        // byte held by a retained worktree plus every orphaned host root.
        let estimated_retained_bytes = orphans
            .iter()
            .map(|orphan| orphan.estimated_bytes)
            .fold(cleanup.estimated_retained_bytes, u64::saturating_add);
        let estimated_retained_inodes = sum_inode_estimates(
            cleanup
                .worktrees
                .iter()
                .map(|worktree| worktree.estimated_inodes)
                .chain(orphans.iter().map(|orphan| orphan.estimated_inodes)),
        );
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
        let worktree_scoped_reclaimable_inodes = sum_inode_estimates(
            worktrees
                .iter()
                .map(|worktree| worktree.estimated_inodes)
                .chain(artifacts.iter().map(|artifact| artifact.estimated_inodes))
                .chain(orphans.iter().map(|orphan| orphan.estimated_inodes)),
        );
        let estimated_blocked_inodes = estimated_retained_inodes
            .zip(worktree_scoped_reclaimable_inodes)
            .map(|(retained, reclaimable)| retained.saturating_sub(reclaimable));

        // Being over budget now decides what a bounded `gc apply` spends its
        // time on, rather than only setting a flag somebody reads (#176).
        // `gc_apply_bounded` drains these lists in order and stops at its
        // deadline, so the front of the list is the part that is real.
        let policy_budget = policy.retained_bytes_budget;
        // Orphaned roots that were not sized are unknown bytes too, so they
        // count against the total's completeness exactly as unmeasured
        // worktrees do. Only `Over` is conclusive from a floor (#176).
        let unmeasured_directory_count =
            cleanup.unmeasured_worktree_count + if scan.measures() { 0 } else { orphans.len() };
        let retained_total = crate::MeasuredTotal {
            bytes: estimated_retained_bytes,
            measured: cleanup
                .worktrees
                .len()
                .saturating_sub(cleanup.unmeasured_worktree_count),
            unmeasured: unmeasured_directory_count,
            oldest_measured_at_ms: cleanup.sizes_measured_at_ms,
        };
        let mut budget_verdict = crate::budget_verdict(&retained_total, policy_budget);
        let reclaim_order =
            crate::reclaim_order::order_for(estimated_retained_bytes, policy_budget);
        crate::reclaim_order::sort_by_order(&mut worktrees, reclaim_order, |worktree| {
            crate::ReclaimRanking {
                id: worktree.session_id,
                retained_since_ms: worktree.closed_at,
                estimated_bytes: worktree.estimated_bytes,
            }
        });
        crate::reclaim_order::sort_by_order(&mut artifacts, reclaim_order, |artifact| {
            crate::ReclaimRanking {
                id: artifact.session_id,
                // Build caches record idleness, not a closing time. Converting
                // it back to an instant keeps one comparison rule for every
                // kind of reclamation instead of a second rule for this one.
                retained_since_ms: evaluated_at
                    .saturating_sub(i64::from(artifact.idle_days).saturating_mul(86_400_000)),
                estimated_bytes: artifact.estimated_bytes,
            }
        });
        // Orphaned roots carry no age -- their owning database is gone, which
        // is what makes them orphans -- so the budget rule has nothing extra
        // to say about them and routing them through it would be ordering
        // theatre. Bytes are the only fact available, and the largest
        // abandoned root is the one a bounded sweep should reach first either
        // way. Path breaks ties so the plan stays byte-identical run to run.
        orphans.sort_by(|left, right| {
            right
                .estimated_bytes
                .cmp(&left.estimated_bytes)
                .then_with(|| left.worktree_root.cmp(&right.worktree_root))
        });

        let reconciliation =
            self.reconcile_worktree_directories_until(scan.measures(), deadline)?;
        if !reconciliation.complete {
            deferred_checks.push("unclaimed_worktrees".into());
        }
        // A partial inventory can only establish that the recorded floor is
        // over budget. It cannot turn an under-budget floor into a pass.
        if !deferred_checks.is_empty() && budget_verdict == crate::BudgetVerdict::Within {
            budget_verdict = crate::BudgetVerdict::Unknown;
        }
        deferred_checks.sort();
        deferred_checks.dedup();
        let clears_retained_bytes_budget = deferred_checks.is_empty()
            && crate::reclaim_order::clears_budget(
                estimated_retained_bytes,
                policy_budget,
                worktree_scoped_reclaimable,
            );

        let mut plan = GcPlan {
            schema_version: GC_PLAN_SCHEMA_VERSION,
            digest: String::new(),
            evaluated_at,
            policy,
            retention_config_warnings,
            rows,
            files,
            worktrees,
            artifacts,
            orphans,
            gate_caches,
            recovery_archives,
            blockers,
            checkpoint_pin_releases,
            publication_exposure_expiries,
            blocker_summary,
            worktree_blocker_summary,
            declined_artifacts,
            estimated_reclaimable_bytes,
            estimated_reclaimable_inodes,
            estimated_retained_bytes,
            estimated_retained_inodes,
            estimated_blocked_bytes,
            estimated_blocked_inodes,
            estimated_declined_artifact_bytes,
            estimated_declined_artifact_inodes,
            gate_cache,
            estimated_build_output_reclaimable_bytes,
            estimated_build_output_reclaimable_inodes,
            reclaim_order,
            retained_bytes_deficit: crate::reclaim_order::deficit_bytes(
                estimated_retained_bytes,
                policy_budget,
            ),
            clears_retained_bytes_budget,
            // Sizing unclaimed directories is another full walk, so it
            // belongs to whichever pass was already paying for walks.
            reconciliation: Some(reconciliation),
            closed_worktrees: crate::retention::ClosedWorktreeSummary::from_cleanup(
                &cleanup,
                &sessions.values().cloned().collect::<Vec<_>>(),
                &main_root,
            ),
            unmeasured_directory_count,
            sizes_measured_at_ms: cleanup.sizes_measured_at_ms,
            budget_verdict,
            recovery_archive_inventory,
            artifact_worktrees_not_scanned,
            deferred_checks: deferred_checks.clone(),
            auto_cleanup: None,
        };
        // Reporting only and outside the digest: what the last unattended
        // pass removed and why it kept the rest (#588).
        plan.auto_cleanup = self.last_auto_cleanup_report();
        if scan.measures() {
            plan.finish_digest()?;
        }
        Ok(plan)
    }

    pub fn gc_apply(&mut self, confirm: &str) -> Result<GcApplyReport, BrokerOpError> {
        self.gc_apply_bounded(confirm, None)
    }

    /// Apply a plan made with `--include-active-gate-cache`. The digest binds
    /// the exact candidate set, so a plain apply of such a digest is refused
    /// as stale rather than silently taking the narrower set.
    pub fn gc_apply_including_active_gate_cache(
        &mut self,
        confirm: &str,
    ) -> Result<GcApplyReport, BrokerOpError> {
        self.gc_apply_with(confirm, None, true)
    }

    /// The retention summary `doctor`, `certify` and the verify loop report.
    ///
    /// All three run unattended, so this takes the recorded-size path. Its
    /// counts are exact; its byte totals are floors, and `budget_verdict` says
    /// which questions those floors can answer (#176).
    pub fn gc_health(&mut self) -> Result<GcHealth, BrokerOpError> {
        let plan = self.gc_plan_recorded()?;
        self.gc_health_for_plan(plan, None)
    }

    pub(crate) fn gc_health_until(
        &mut self,
        deadline: std::time::Instant,
    ) -> Result<GcHealth, BrokerOpError> {
        let plan = self.gc_plan_scanned_until(crate::SizeScan::Recorded, false, Some(deadline))?;
        self.gc_health_for_plan(plan, Some(deadline))
    }

    fn gc_health_for_plan(
        &self,
        mut plan: GcPlan,
        deadline: Option<std::time::Instant>,
    ) -> Result<GcHealth, BrokerOpError> {
        let journal = if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            plan.deferred_checks.push("gc_journal".into());
            None
        } else {
            load_journal(&self.main_root().join(".aethyme/gc-journal.json"))?
        };
        plan.deferred_checks.sort();
        plan.deferred_checks.dedup();
        if !plan.deferred_checks.is_empty() {
            if plan.budget_verdict == crate::BudgetVerdict::Within {
                plan.budget_verdict = crate::BudgetVerdict::Unknown;
            }
            plan.clears_retained_bytes_budget = false;
        }
        let over_retained_bytes_budget = plan.budget_verdict.exceeded();
        Ok(GcHealth {
            deferred_checks: plan.deferred_checks,
            policy: plan.policy,
            retention_config_warnings: plan.retention_config_warnings,
            pending_recovery_digest: journal.map(|journal| journal.digest),
            candidate_rows: plan.rows.len(),
            candidate_files: plan.files.len(),
            candidate_worktrees: plan.worktrees.len(),
            candidate_artifacts: plan.artifacts.len(),
            candidate_orphans: plan.orphans.len(),
            estimated_reclaimable_bytes: plan.estimated_reclaimable_bytes,
            estimated_reclaimable_inodes: plan.estimated_reclaimable_inodes,
            estimated_retained_bytes: plan.estimated_retained_bytes,
            estimated_retained_inodes: plan.estimated_retained_inodes,
            estimated_blocked_bytes: plan.estimated_blocked_bytes,
            estimated_blocked_inodes: plan.estimated_blocked_inodes,
            over_retained_bytes_budget,
            retained_bytes_deficit: plan.retained_bytes_deficit,
            clears_retained_bytes_budget: plan.clears_retained_bytes_budget,
            reclaim_order: plan.reclaim_order,
            budget_verdict: plan.budget_verdict,
            unmeasured_directory_count: plan.unmeasured_directory_count,
            artifact_worktrees_not_scanned: plan.artifact_worktrees_not_scanned,
            sizes_measured_at_ms: plan.sizes_measured_at_ms,
            blockers: plan.blockers.len(),
            unclaimed_worktree_count: plan
                .reconciliation
                .as_ref()
                .map_or(0, |sweep| sweep.unclaimed_count),
            unclaimed_worktree_bytes: plan
                .reconciliation
                .as_ref()
                .map_or(0, |sweep| sweep.unclaimed_bytes),
            closed_worktrees: plan.closed_worktrees,
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
        let continuation_worker = std::env::var_os(ARTIFACT_SWEEP_WORKER_ENV).is_some();
        if continuation_worker {
            // This process is deliberately cache-only. In particular, do not
            // resume an unrelated authorized checkout-removal journal from
            // Broker::open before the worker's own scoped pass.
            return Ok(None);
        }
        let policy = load_retention_policy(self.main_root())?;
        let journal_path = self.main_root().join(".aethyme/gc-journal.json");
        let resumed = match load_journal(&journal_path)? {
            Some(journal) => {
                Some(self.gc_apply_bounded(&journal.digest, Some(policy.startup_budget_ms))?)
            }
            None => None,
        };
        // The sweep only reclaims rebuildable caches, and this runs inside
        // broker open: its failure must not stop every command.
        let _ = self.sweep_artifacts_autonomously(&policy, ArtifactSweepScope::Inline);
        // Inline admission stays capped at 250 ms. A detached, installed
        // worker continues only the durable cache sweep when that pass leaves
        // an unfinished lap or a pressure-triggered request is pending.
        let _ = self.maybe_start_artifact_sweep_worker(false);
        Ok(resumed)
    }

    /// Run the autonomous artifact sweep now with the full policy budget,
    /// scaled by disk pressure, regardless of cadence. Safety rules are the
    /// ones broker open applies; only the budget and the schedule differ.
    pub fn gc_sweep_artifacts(&mut self) -> Result<ArtifactSweepReport, BrokerOpError> {
        let policy = load_retention_policy(self.main_root())?;
        let (budget_ms, urgency) = self.artifact_sweep_budget(&policy, ArtifactSweepScope::Drain);
        let outcome = self.sweep_artifacts_autonomously(&policy, ArtifactSweepScope::Drain)?;
        Ok(ArtifactSweepReport {
            directories_reclaimed: outcome.directories_reclaimed,
            complete: outcome.complete,
            budget_ms,
            disk_pressured: urgency == crate::disk_headroom::SweepUrgency::Pressured,
            auto_cleanup: outcome.auto_cleanup,
        })
    }

    /// Continue an unfinished sweep with the full budget, if one is pending.
    /// `finish` calls this: an agent finishing is about to leave anyway, so
    /// the drain the inline path no longer does is paid there instead.
    pub(crate) fn continue_artifact_sweep_backlog(
        &mut self,
    ) -> Result<Option<ArtifactSweepOutcome>, BrokerOpError> {
        if !self.artifact_sweep_backlog_pending()? {
            return Ok(None);
        }
        let policy = load_retention_policy(self.main_root())?;
        self.sweep_artifacts_autonomously(&policy, ArtifactSweepScope::Drain)
            .map(Some)
    }

    /// Persist an automatic recovery request when an admission failure or an
    /// unfinished bounded sweep has exposed durable cache backlog. The marker
    /// is intentionally separate from the cadence/cursor state: normal broker
    /// opens can request a worker without pretending that the foreground pass
    /// finished or resetting its resume point.
    pub(crate) fn request_artifact_sweep_recovery(&mut self) -> Result<(), BrokerOpError> {
        self.persist_artifact_sweep_recovery_request()?;
        self.start_artifact_sweep_recovery_worker()
    }

    /// Record the durable request without launching the detached process.
    /// Preparation uses this while holding its shared-cache lease, then starts
    /// the worker only after the lease has been released successfully.
    pub(crate) fn persist_artifact_sweep_recovery_request(&mut self) -> Result<(), BrokerOpError> {
        let prior = self
            .store()
            .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        // Treat every pressure refusal as a fresh generation. The worker
        // compares this token before clearing its request, so a new refusal
        // racing the end of a complete lap cannot disappear.
        let requested_at = now_ms().max(prior.saturating_add(1)).max(1);
        self.store().meta_set(
            ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY,
            &requested_at.to_string(),
        )?;
        self.store().meta_set(ARTIFACT_SWEEP_RETRY_AFTER_KEY, "")?;
        Ok(())
    }

    /// Launch recovery after the request is durable and any conflicting
    /// preparation lease has been released.
    pub(crate) fn start_artifact_sweep_recovery_worker(&mut self) -> Result<(), BrokerOpError> {
        let _ = self.maybe_start_artifact_sweep_worker(true);
        Ok(())
    }

    /// Launch the installed router as a detached cache-only continuation.
    /// The OS advisory lock is the singleton; the PID text is diagnostic only.
    fn maybe_start_artifact_sweep_worker(
        &mut self,
        pressure_request: bool,
    ) -> Result<(), BrokerOpError> {
        let root = self.main_root().to_path_buf();
        if crate::host_state::path_is_ephemeral(&root)
            || artifact_worker_process_is_suppressed()
            || !self.artifact_sweep_backlog_pending()?
        {
            return Ok(());
        }
        if !pressure_request && worker_retry_pending(self.store(), now_ms())? {
            return Ok(());
        }
        let Some((router, _engine)) = artifact_worker_binaries() else {
            self.defer_artifact_sweep_worker("runtime_unverified");
            return Ok(());
        };
        let Some(lock) = try_artifact_sweep_worker_lock(&root)? else {
            return Ok(());
        };
        if !artifact_worker_runtime_is_aligned() {
            self.defer_artifact_sweep_worker("runtime_pair_unverified");
            return Ok(());
        }
        let canonical_root =
            std::fs::canonicalize(&root).map_err(|source| crate::BrokerError::Io {
                path: root.clone(),
                source,
            })?;
        let mut child = match Command::new(&router)
            .args(["broker", "gc", "__continue-artifacts"])
            .arg(&canonical_root)
            .current_dir(&canonical_root)
            .env(ARTIFACT_SWEEP_WORKER_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => {
                self.defer_artifact_sweep_worker("worker_launch_failed");
                return Ok(());
            }
        };
        let holder = format!("pid={}", child.id());
        if lock.record_holder(&holder).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            self.defer_artifact_sweep_worker("worker_lock_record_failed");
        }
        // Dropping the parent lock lets the child (which already exists and
        // is blocked on the same OS lock) take over without a PID race.
        drop(lock);
        Ok(())
    }

    /// Execute the detached worker's bounded, cache-only continuation loop.
    /// It exits after three passes without an actual unlink, leaves the
    /// durable request/cursor in place, and backs off so an open file or
    /// concurrent writer cannot keep a background process alive forever.
    pub(crate) fn run_artifact_sweep_recovery_worker(&mut self) -> Result<(), BrokerOpError> {
        if !matches!(std::env::var(ARTIFACT_SWEEP_WORKER_ENV).as_deref(), Ok("1")) {
            self.write_artifact_sweep_worker_status("refused_environment");
            return Ok(());
        }
        if crate::host_state::path_is_ephemeral(self.main_root()) {
            self.write_artifact_sweep_worker_status("refused_ephemeral_repository");
            return Ok(());
        }
        if !self.artifact_sweep_backlog_pending()? {
            self.write_artifact_sweep_worker_status("no_backlog");
            return Ok(());
        }
        if worker_retry_pending(self.store(), now_ms())? {
            self.write_artifact_sweep_worker_status("retry_deferred");
            return Ok(());
        }
        if !artifact_worker_runtime_is_aligned() {
            self.defer_artifact_sweep_worker("runtime_pair_unverified");
            return Ok(());
        }
        let policy = load_retention_policy(self.main_root())?;
        if policy.artifact_sweep_budget_ms == 0 {
            self.defer_artifact_sweep_worker("artifact_sweep_disabled");
            return Ok(());
        }
        let request_generation = self
            .store()
            .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);

        self.run_artifact_sweep_recovery_worker_loop(
            &policy,
            request_generation,
            None,
            |broker, policy| {
                broker.sweep_artifacts_autonomously_with_progress(
                    policy,
                    ArtifactSweepScope::AutomaticContinuation,
                )
            },
        )
    }

    /// The durable worker loop is separated from the installed-runtime gate
    /// so child-process tests can drive its real generation/cursor behavior
    /// with the test harness binary. Production always calls it through
    /// `run_artifact_sweep_recovery_worker` after runtime validation.
    fn run_artifact_sweep_recovery_worker_loop(
        &mut self,
        policy: &RetentionPolicy,
        request_generation: i64,
        max_passes: Option<usize>,
        mut sweep_pass: impl FnMut(
            &mut Self,
            &RetentionPolicy,
        ) -> Result<(ArtifactSweepOutcome, usize), BrokerOpError>,
    ) -> Result<(), BrokerOpError> {
        let mut passes = 0usize;
        let mut no_progress_passes = 0usize;
        let mut entries_reclaimed = 0usize;
        loop {
            passes = passes.saturating_add(1);
            let (outcome, pass_entries_reclaimed) = match sweep_pass(self, policy) {
                Ok(outcome) => outcome,
                Err(_) => {
                    self.defer_artifact_sweep_worker("sweep_failed");
                    return Ok(());
                }
            };
            entries_reclaimed = entries_reclaimed.saturating_add(pass_entries_reclaimed);
            if outcome.complete {
                // Preserve request history and advance only the generation
                // this pass actually covered. A pressure refusal arriving
                // between this write and the next open remains pending.
                self.store().meta_set(
                    ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY,
                    &request_generation.to_string(),
                )?;
                self.store().meta_set(ARTIFACT_SWEEP_RETRY_AFTER_KEY, "")?;
                self.write_artifact_sweep_worker_status(&format!(
                    "complete passes={passes} entries={entries_reclaimed}"
                ));
                return Ok(());
            }
            if max_passes.is_some_and(|limit| passes >= limit) {
                // This seam lets a child-process test stop after one real
                // incomplete lap, then prove another process can resume from
                // its persisted cursor without manufacturing completion.
                return Ok(());
            }
            if pass_entries_reclaimed == 0 {
                no_progress_passes = no_progress_passes.saturating_add(1);
                if no_progress_passes >= ARTIFACT_SWEEP_WORKER_NO_PROGRESS_PASSES {
                    self.defer_artifact_sweep_worker(&format!(
                        "no_progress passes={passes} entries={entries_reclaimed}"
                    ));
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(1_000));
            } else {
                no_progress_passes = 0;
            }
        }
    }

    fn defer_artifact_sweep_worker(&self, reason: &str) {
        let retry_after = now_ms().saturating_add(ARTIFACT_SWEEP_WORKER_RETRY_MS);
        let _ = self
            .store()
            .meta_set(ARTIFACT_SWEEP_RETRY_AFTER_KEY, &retry_after.to_string());
        self.write_artifact_sweep_worker_status(reason);
    }

    fn write_artifact_sweep_worker_status(&self, status: &str) {
        // The value is intentionally bounded and replaced rather than logged
        // to a file that could itself grow without limit.
        let bounded = status.chars().take(160).collect::<String>();
        let _ = self
            .store()
            .meta_set(ARTIFACT_SWEEP_WORKER_STATUS_KEY, &bounded);
    }

    /// Whether a recovery request remains durable. The worker advances a
    /// separate completed generation only after a complete cache-only lap;
    /// interruptions retain this marker and the existing sweep cursor/tree.
    pub(crate) fn artifact_sweep_recovery_requested(&self) -> Result<bool, BrokerOpError> {
        let requested = self
            .store()
            .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = self
            .store()
            .meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        Ok(requested > completed)
    }

    /// An earlier pass stopped at its budget and has not finished a lap since.
    fn artifact_sweep_backlog_pending(&mut self) -> Result<bool, BrokerOpError> {
        let cursor = self
            .store()
            .meta_get(ARTIFACT_SWEEP_CURSOR_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let partial = self
            .store()
            .meta_get(ARTIFACT_SWEEP_PROGRESS_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        Ok(cursor != 0 || partial != 0 || self.artifact_sweep_recovery_requested()?)
    }

    /// Whether the millisecond stamp under `key` is set and younger than
    /// `window_ms`.
    fn meta_stamp_within(
        &mut self,
        key: &str,
        now: i64,
        window_ms: i64,
    ) -> Result<bool, BrokerOpError> {
        Ok(self
            .store()
            .meta_get(key)?
            .and_then(|value| value.parse::<i64>().ok())
            .is_some_and(|last| last > 0 && now.saturating_sub(last) < window_ms))
    }

    fn artifact_sweep_budget(
        &self,
        policy: &RetentionPolicy,
        scope: ArtifactSweepScope,
    ) -> (u64, crate::disk_headroom::SweepUrgency) {
        let main_root = self.main_root();
        // The detached worker can reclaim caches from external worktree and
        // preparation-cache volumes. Looking only at the main checkout misses
        // pressure on those filesystems (the main checkout is often elsewhere).
        let mut roots = BTreeSet::from([main_root.to_path_buf()]);
        if scope == ArtifactSweepScope::AutomaticContinuation {
            if let Ok(sessions) = self.store().live_sessions() {
                roots.extend(
                    sessions
                        .into_iter()
                        .map(|session| PathBuf::from(session.worktree_path)),
                );
            }
            if let Ok(sessions) = self.store().cleaned_sessions() {
                roots.extend(
                    sessions
                        .into_iter()
                        .map(|session| PathBuf::from(session.worktree_path)),
                );
            }
            if let Ok(cache_root) = crate::preparation::preparation_cache_root() {
                roots.insert(cache_root);
            }
        }
        // One free-space sample per filesystem: many hundreds of session
        // roots may share the same mounted volume.
        let mut sampled_devices = BTreeSet::new();
        let mut urgency = crate::disk_headroom::SweepUrgency::Routine;
        for path in roots {
            let Some(existing) = path.ancestors().find(|ancestor| ancestor.exists()) else {
                continue;
            };
            #[cfg(unix)]
            let device = {
                use std::os::unix::fs::MetadataExt;
                let Ok(metadata) = std::fs::metadata(existing) else {
                    continue;
                };
                metadata.dev()
            };
            #[cfg(not(unix))]
            let device = existing.to_path_buf();
            if !sampled_devices.insert(device) {
                continue;
            }
            let headroom =
                crate::disk_headroom::available_headroom_at_or_above_for(main_root, &path);
            if crate::disk_headroom::sweep_urgency_with_inodes(
                headroom.map(|value| value.bytes),
                crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES,
                headroom.and_then(|value| value.inodes),
                crate::disk_headroom::MIN_GATE_HEADROOM_INODES,
            ) == crate::disk_headroom::SweepUrgency::Pressured
            {
                urgency = crate::disk_headroom::SweepUrgency::Pressured;
                break;
            }
        }
        (
            artifact_sweep_budget_ms(policy.artifact_sweep_budget_ms, scope, urgency),
            urgency,
        )
    }

    /// Reclaim build caches from one just-closed session while retaining its
    /// checkout and branch. The periodic sweep cadence does not delay this
    /// targeted pass.
    pub(crate) fn reclaim_closed_session_artifacts(
        &mut self,
        session_id: i64,
    ) -> Result<ArtifactSweepOutcome, BrokerOpError> {
        let policy = load_retention_policy(self.main_root())?;
        self.sweep_artifacts_autonomously(&policy, ArtifactSweepScope::ClosedSession(session_id))
    }

    /// Reclaim build caches from long-idle closed worktrees without operator
    /// confirmation.
    ///
    /// Open sessions are included once their agent has shown no evidence of
    /// work for `idle_session_artifact_hours` -- the abandonment rule over a
    /// shorter window, because losing build output costs a rebuild while
    /// abandonment releases the checkout. A session inside the 72-hour
    /// abandonment window otherwise holds its caches for three days after its
    /// agent left; observed, 8.3 GiB across three quiet sessions of one
    /// repository. `gc plan` does not list these: its candidates are
    /// re-validated at apply against closed sessions, and the reviewed lane for
    /// an open session's build output is `gc reclaim`, which keeps only
    /// sessions that are actively working.
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
    /// and records the session it stopped after so the next
    /// pass continues from there. Resuming without that cursor re-scans the
    /// head of the list every time, which is why a backlog larger than one
    /// budget used to sit untouched behind it (#222).
    fn sweep_artifacts_autonomously(
        &mut self,
        policy: &RetentionPolicy,
        scope: ArtifactSweepScope,
    ) -> Result<ArtifactSweepOutcome, BrokerOpError> {
        self.sweep_artifacts_autonomously_with_progress(policy, scope)
            .map(|(outcome, _)| outcome)
    }

    fn sweep_artifacts_autonomously_with_progress(
        &mut self,
        policy: &RetentionPolicy,
        scope: ArtifactSweepScope,
    ) -> Result<(ArtifactSweepOutcome, usize), BrokerOpError> {
        self.sweep_artifacts_autonomously_with_deadline(policy, scope, None)
    }

    fn sweep_artifacts_autonomously_with_deadline(
        &mut self,
        policy: &RetentionPolicy,
        scope: ArtifactSweepScope,
        deadline_override: Option<Instant>,
    ) -> Result<(ArtifactSweepOutcome, usize), BrokerOpError> {
        if policy.artifact_sweep_budget_ms == 0 {
            return Ok((
                ArtifactSweepOutcome {
                    directories_reclaimed: 0,
                    complete: true,
                    auto_cleanup: None,
                },
                0,
            ));
        }
        let closed_session_id = match scope {
            ArtifactSweepScope::ClosedSession(session_id) => Some(session_id),
            ArtifactSweepScope::Inline
            | ArtifactSweepScope::Drain
            | ArtifactSweepScope::AutomaticContinuation => None,
        };
        let main_root = self.main_root().to_path_buf();
        let now = now_ms();
        let (budget_ms, urgency) = self.artifact_sweep_budget(policy, scope);
        // The deadline covers discovery as well as removal: scanning dozens of
        // worktrees is itself work a broker open must not pay unbounded.
        let deadline =
            deadline_override.unwrap_or_else(|| Instant::now() + Duration::from_millis(budget_ms));
        if scope == ArtifactSweepScope::Inline {
            let interval_ms = (i64::from(policy.artifact_sweep_interval_hours) * 3_600_000)
                / urgency.interval_divisor().max(1);
            let spacing_ms = INLINE_PARTIAL_PASS_SPACING_MS / urgency.interval_divisor().max(1);
            if self.meta_stamp_within(ARTIFACT_SWEEP_STAMP_KEY, now, interval_ms)?
                || self.meta_stamp_within(ARTIFACT_SWEEP_PROGRESS_KEY, now, spacing_ms)?
            {
                return Ok((
                    ArtifactSweepOutcome {
                        directories_reclaimed: 0,
                        complete: true,
                        auto_cleanup: None,
                    },
                    0,
                ));
            }
        }
        // A concurrent GC owns the artifact namespace; skip rather than race.
        let Ok(_lock) = GcLock::acquire(&main_root) else {
            return Ok((
                ArtifactSweepOutcome {
                    directories_reclaimed: 0,
                    complete: closed_session_id.is_none()
                        && scope != ArtifactSweepScope::AutomaticContinuation,
                    auto_cleanup: None,
                },
                0,
            ));
        };
        let live_sessions = self.store().live_sessions()?;
        let live = live_sessions
            .iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        let live_worktrees = live_sessions
            .iter()
            .map(|session| PathBuf::from(&session.worktree_path))
            .collect::<Vec<_>>();
        // Open sessions whose agent has been gone for the idle window. They
        // stay open and keep their checkout; only their witnessed build output
        // is swept. The targeted pass after a close is about one closed
        // session and never widens to these.
        let idle_window_ms = i64::from(policy.idle_session_artifact_hours) * 3_600_000;
        let idle_open = if closed_session_id.is_none() && idle_window_ms > 0 {
            self.idle_open_sessions(now, idle_window_ms)?
        } else {
            Vec::new()
        };
        let idle_open_ids = idle_open
            .iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        let mut preparation_entries_reclaimed = 0usize;
        let mut scan_completed = true;
        if closed_session_id.is_none() {
            // The shared preparation cache is content-addressed: an entry no
            // checkout computes is unreadable forever, so it needs no operator
            // review. It was reachable only from the manual lane, which is why a
            // hand reclaim of 13.3 GB returned as 15 GB within three days.
            let preparation_sweep = crate::storage::sweep_preparation_cache(
                &main_root,
                policy.orphan_worktree_roots_days,
                deadline,
            );
            preparation_entries_reclaimed = preparation_sweep.filesystem_entries_reclaimed;
            scan_completed = preparation_sweep.complete;
            if preparation_sweep.entries_removed > 0 {
                let payload = serde_json::json!({
                    "entries": preparation_sweep.entries_removed,
                    "bytes": preparation_sweep.bytes_reclaimed,
                    "urgency": format!("{urgency:?}"),
                    "complete": preparation_sweep.complete,
                })
                .to_string();
                self.store().append_event(
                    crate::events::BROKER_GC_PREPARATION_SWEPT,
                    None,
                    Some(&payload),
                )?;
            }
        }
        let mut removed = Vec::new();
        let mut entries_reclaimed = preparation_entries_reclaimed;
        let mut eligible_worktree_seen = false;
        let cursor = self
            .store()
            .meta_get(ARTIFACT_SWEEP_CURSOR_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let mut last_visited = cursor;
        let mut all = self.store().cleaned_sessions()?;
        let sessions = match closed_session_id {
            Some(session_id) => all
                .iter()
                .filter(|session| session.id == session_id)
                .cloned()
                .collect(),
            None => {
                // One id-ordered list, so the cursor resumes across both kinds.
                all.extend(idle_open);
                all.sort_by_key(|session| session.id);
                sweep_order(&all, cursor, |session| session.id)
            }
        };
        let mut idle_open_removed = 0usize;
        // Forget a remembered tree that no longer exists, whoever removed it.
        let mut interrupted_path = self
            .store()
            .meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)?
            .filter(|path| !path.is_empty() && is_real_directory(Path::new(path)));
        let mut visited_any = false;
        for session in sessions {
            // Checked before the session is taken, so the cursor names the
            // last session actually visited and the next pass starts here.
            // The first session is always visited: a budget spent before the
            // loop must still buy ground, or a backlog never converges.
            if visited_any && check_deadline(Some(deadline)) {
                scan_completed = false;
                break;
            }
            visited_any = true;
            last_visited = session.id;
            let is_idle_open = idle_open_ids.contains(&session.id);
            let root = PathBuf::from(&session.worktree_path);
            let sheltered = if is_idle_open {
                // Its own row is live, so it must not shelter itself; every
                // other live session still does.
                let others = live_sessions
                    .iter()
                    .filter(|live| live.id != session.id)
                    .map(|live| PathBuf::from(&live.worktree_path))
                    .collect::<Vec<_>>();
                overlaps_live_worktree(&root, &others)
            } else {
                if live.contains(&session.id) {
                    continue;
                }
                let closed_at = session.closed_at.unwrap_or(session.updated_at);
                if days_between(now, closed_at) < policy.artifact_reclaim_days {
                    continue;
                }
                overlaps_live_worktree(&root, &live_worktrees)
            };
            if sheltered
                || !is_real_directory(&root)
                || !self.is_broker_owned_worktree(&session, &root)
            {
                continue;
            }
            let Ok(checkout) = GitRepo::discover(&root) else {
                continue;
            };
            eligible_worktree_seen = true;
            let mut found = Vec::new();
            collect_artifact_dirs_with_extras(
                &root,
                &root,
                0,
                &policy.artefact_directories,
                &mut found,
            );
            // The tree this sweep was part-way through goes first and is found
            // by path: a cache witnessed only by being non-empty stops
            // classifying once a paused removal has emptied it, and taking it
            // first means a pause elsewhere can never replace -- and so forget
            // -- the one remembered tree.
            if let Some(path) = interrupted_path.as_deref().map(PathBuf::from)
                && path.starts_with(&root)
            {
                if resumable_interrupted_tree(&root, &path, &policy.artefact_directories) {
                    found.retain(|dir| dir != &path);
                    found.insert(0, path);
                } else {
                    // Lexically claims this worktree but is not a build cache
                    // inside it: never act on it, and stop remembering it.
                    interrupted_path = None;
                }
            }
            for dir in found {
                let Some(relative) = repo_relative(&root, &dir) else {
                    continue;
                };
                if !checkout.path_is_ignored(&relative) {
                    continue;
                }
                // The session is open, and a quiet agent is not the only
                // thing that writes build output: a dev server or a long
                // build can outlive the agent's last commit. Output touched
                // inside the idle window is still in use.
                let dir_key = dir.to_string_lossy().into_owned();
                let resuming = interrupted_path.as_deref() == Some(dir_key.as_str());
                if is_idle_open && !resuming && modified_within(&dir, now, idle_window_ms) {
                    continue;
                }
                let deferrable =
                    artifact_witness_for_with_extras(&dir, &policy.artefact_directories)
                        .and_then(ArtifactWitness::deferrable_entry);
                match remove_condemned_tree_with_progress(&dir, deferrable, Some(deadline)) {
                    Ok((TreeRemoval::Complete, count)) => {
                        entries_reclaimed = entries_reclaimed.saturating_add(count);
                        if resuming {
                            interrupted_path = None;
                        }
                        removed.push(dir_key);
                        if is_idle_open {
                            idle_open_removed += 1;
                        }
                    }
                    // The budget stopped a removal partway. The directory is
                    // still a recognisable build cache, so the next pass finds
                    // it again and carries on -- and withholding the cadence
                    // stamp below is what makes a next pass happen today
                    // rather than after the interval.
                    Ok((TreeRemoval::Interrupted, count)) => {
                        entries_reclaimed = entries_reclaimed.saturating_add(count);
                        // One remembered tree at a time, and never replaced
                        // before it is finished: the next pass starts after
                        // this session, so a pause elsewhere would otherwise
                        // overwrite it before it is resumed.
                        if interrupted_path.is_none() {
                            interrupted_path = Some(dir_key);
                        }
                        scan_completed = false;
                    }
                    Err(error) => {
                        entries_reclaimed =
                            entries_reclaimed.saturating_add(error.entries_reclaimed);
                        scan_completed = false;
                        if closed_session_id.is_none() {
                            // The failed session must be first on the next
                            // pass. Otherwise the cursor could skip it and
                            // later report a complete cache lap.
                            last_visited = session.id.saturating_sub(1);
                        }
                    }
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
        // A full lap visited every eligible session, so a remembered tree it
        // did not resume is no longer eligible; holding it would only stop
        // the next pause from being remembered.
        if scan_completed && closed_session_id.is_none() {
            interrupted_path = None;
        }
        match &interrupted_path {
            Some(path) => self
                .store()
                .meta_set(ARTIFACT_SWEEP_INTERRUPTED_KEY, path)?,
            None => self.store().meta_set(ARTIFACT_SWEEP_INTERRUPTED_KEY, "")?,
        }
        // Do not consume the cadence window before any closed worktree exists,
        // and do not hide an unfinished backlog for a full interval. Removal
        // failures do consume the window, preventing a broken path from
        // slowing every broker command until an operator can inspect it.
        if let Some(session_id) = closed_session_id {
            if !scan_completed {
                // Retry this session first on the next ordinary broker open.
                self.store().meta_set(
                    ARTIFACT_SWEEP_CURSOR_KEY,
                    &session_id.saturating_sub(1).to_string(),
                )?;
                self.store().meta_set(ARTIFACT_SWEEP_STAMP_KEY, "0")?;
            }
        } else {
            if eligible_worktree_seen && scan_completed {
                self.store()
                    .meta_set(ARTIFACT_SWEEP_STAMP_KEY, &now.to_string())?;
            }
            // A finished lap starts the next one from the top; an interrupted pass
            // remembers where it stopped so the next one advances instead of
            // re-walking what it already cleared, and when, so broker opens
            // space their continuations instead of each paying a budget.
            if scan_completed {
                self.store().meta_set(ARTIFACT_SWEEP_CURSOR_KEY, "0")?;
                self.store().meta_set(ARTIFACT_SWEEP_PROGRESS_KEY, "0")?;
            } else {
                self.store()
                    .meta_set(ARTIFACT_SWEEP_CURSOR_KEY, &last_visited.to_string())?;
                self.store()
                    .meta_set(ARTIFACT_SWEEP_PROGRESS_KEY, &now.to_string())?;
            }
        }
        if !removed.is_empty() {
            let payload = serde_json::json!({
                "directories": removed.len(),
                "idle_days": policy.artifact_reclaim_days,
                "closed_session_directories": removed.len() - idle_open_removed,
                "idle_open_session_directories": idle_open_removed,
                "idle_session_artifact_hours": policy.idle_session_artifact_hours,
            })
            .to_string();
            self.store().append_event(
                crate::events::BROKER_GC_ARTIFACTS_SWEPT,
                None,
                Some(&payload),
            )?;
        }
        // Whole checkouts go after their caches, under the same lock and
        // budget, and only on the general sweep: the targeted pass after a
        // close is about one session's build output (#588). Its failure must
        // not stop the sweep any more than the sweep's may stop a command.
        let auto_cleanup = if closed_session_id.is_none() && scope_runs_checkout_cleanup(scope) {
            self.auto_remove_disposable_checkouts(Some(deadline)).ok()
        } else {
            None
        };
        Ok((
            ArtifactSweepOutcome {
                directories_reclaimed: removed.len(),
                complete: scan_completed,
                auto_cleanup,
            },
            entries_reclaimed,
        ))
    }

    /// Apply or resume an authorized plan. A budget is used by amortized
    /// maintenance; `None` runs until completion or a concrete artifact
    /// failure. Progress is journaled after every bounded batch/item.
    pub fn gc_apply_bounded(
        &mut self,
        confirm: &str,
        budget_ms: Option<u64>,
    ) -> Result<GcApplyReport, BrokerOpError> {
        self.gc_apply_with(confirm, budget_ms, false)
    }

    fn gc_apply_with(
        &mut self,
        confirm: &str,
        budget_ms: Option<u64>,
        include_active_gate_cache: bool,
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
                let plan =
                    self.gc_plan_scanned(crate::SizeScan::Measure, include_active_gate_cache)?;
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

        while !journal.remaining_checkpoint_pin_releases.is_empty() && !check_deadline(deadline) {
            let candidate = journal.remaining_checkpoint_pin_releases[0].clone();
            let consume = match self.store().release_gc_checkpoint_pin(&candidate) {
                Ok(true) => {
                    journal.checkpoint_pins_released.push(candidate.session_id);
                    true
                }
                Ok(false) => {
                    failures.push(format!(
                        "session {} checkpoint pin changed; review a new GC plan",
                        candidate.session_id
                    ));
                    true
                }
                Err(error) => {
                    failures.push(format!(
                        "session {} checkpoint pin release failed: {error}",
                        candidate.session_id
                    ));
                    false
                }
            };
            if !consume {
                break;
            }
            journal.remaining_checkpoint_pin_releases.remove(0);
            write_journal(&journal_path, &journal)?;
        }

        while !journal.remaining_publication_exposure_expiries.is_empty()
            && !check_deadline(deadline)
        {
            let candidate = journal.remaining_publication_exposure_expiries[0].clone();
            let consume = match self.store().expire_gc_publication_exposure(&candidate) {
                Ok(true) => {
                    journal
                        .publication_exposures_expired
                        .push(candidate.exposure_id);
                    true
                }
                Ok(false) => {
                    failures.push(format!(
                        "publication exposure {} changed; review a new GC plan",
                        candidate.exposure_id
                    ));
                    true
                }
                Err(error) => {
                    failures.push(format!(
                        "publication exposure {} expiry failed: {error}",
                        candidate.exposure_id
                    ));
                    false
                }
            };
            if !consume {
                break;
            }
            journal.remaining_publication_exposure_expiries.remove(0);
            write_journal(&journal_path, &journal)?;
        }

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
            // A resumed journal was authorized earlier, so each entry is
            // re-proven here -- from its own session row, without the full
            // size walk a whole `cleanup_plan()` costs per iteration. A
            // checkout that became live since is retained and dropped from
            // the journal rather than pinning it: nothing a later run could do
            // would make removing a live checkout right.
            let session = self.store().session(candidate.session_id)?;
            let retained_live = |failures: &mut Vec<String>| {
                failures.push(format!(
                    "session {}: its worktree is now the checkout of a live session and was retained; review a new GC plan",
                    candidate.session_id
                ));
            };
            if !session.status.is_closed() {
                retained_live(&mut failures);
                journal.remaining_worktrees.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            }
            let mut records = crate::measurement::SizeRecords::default();
            let current = if session.origin == crate::SessionOrigin::Spawned {
                self.cleanup_item_scanned(&session, crate::SizeScan::Recorded, &mut records)?
            } else {
                None
            };
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
                // Immediately before removal, with no walk in between.
                // `cleanup` repeats this check right before it removes, which
                // is the guard every caller shares; this one keeps the entry
                // from pinning the journal.
                let live_worktrees = self
                    .store()
                    .live_sessions()?
                    .iter()
                    .map(|session| PathBuf::from(&session.worktree_path))
                    .collect::<Vec<_>>();
                if candidate.worktree_present
                    && overlaps_live_worktree(Path::new(&candidate.worktree_path), &live_worktrees)
                {
                    retained_live(&mut failures);
                    journal.remaining_worktrees.remove(0);
                    write_journal(&journal_path, &journal)?;
                    continue;
                }
                match self.cleanup(candidate.session_id, false) {
                    Ok(()) => {}
                    Err(BrokerOpError::WorktreeInUseByLiveSession { .. }) => {
                        retained_live(&mut failures);
                        journal.remaining_worktrees.remove(0);
                        write_journal(&journal_path, &journal)?;
                        continue;
                    }
                    Err(error) => {
                        failures.push(format!("session {}: {error}", candidate.session_id));
                        break;
                    }
                }
            }
            journal.reclaimed_bytes = journal
                .reclaimed_bytes
                .saturating_add(candidate.estimated_bytes);
            journal.sessions_cleaned.push(candidate.session_id);
            journal.remaining_worktrees.remove(0);
            write_journal(&journal_path, &journal)?;
        }

        let live_sessions = self.store().live_sessions()?;
        let live = live_sessions
            .iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        let live_worktrees = live_sessions
            .iter()
            .map(|session| PathBuf::from(&session.worktree_path))
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
            if overlaps_live_worktree(&root, &live_worktrees) {
                failures.push(format!(
                    "{}: a live session now works in {}",
                    candidate.relative_dir, candidate.worktree_path
                ));
                journal.remaining_artifacts.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            }
            let Ok(checkout) = GitRepo::discover(&root) else {
                failures.push(format!(
                    "{}: its checkout disappeared; review a new GC plan",
                    candidate.relative_dir
                ));
                journal.remaining_artifacts.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            };
            if !checkout.path_is_ignored(&candidate.relative_dir) {
                failures.push(format!(
                    "{}: is no longer git-ignored; review a new GC plan",
                    candidate.relative_dir
                ));
                journal.remaining_artifacts.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            }
            let Some(witness) =
                artifact_witness_for_with_extras(&dir, &journal.policy.artefact_directories)
            else {
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

        // Each entry re-proves at removal that no gate uses it, taking the
        // entry's own lease to do so, and that it is still the entry the plan
        // measured; one that fails is retained and the run carries on,
        // because gate cache entries share no fate (#295).
        //
        // Only an operator-invoked apply reaches this. Re-proving an entry
        // walks it -- gigabytes -- which no startup budget absorbs, and the
        // unattended resume on broker open must not be what decides to
        // discard a cache. A journal that still lists gate caches stays
        // incomplete and names the `gc apply --confirm` that finishes it.
        while budget_ms.is_none()
            && !journal.remaining_gate_caches.is_empty()
            && !check_deadline(deadline)
        {
            let candidate = journal.remaining_gate_caches[0].clone();
            match crate::gate_cache_gc::reclaim(&main_root, &candidate, deadline) {
                Ok(crate::gate_cache_gc::GateCacheReclaim::Removed(bytes)) => {
                    journal.reclaimed_bytes = journal.reclaimed_bytes.saturating_add(bytes);
                    journal.gate_caches_reclaimed.push(candidate.path);
                }
                // Gone is the outcome asked for; this run did not free it.
                Ok(crate::gate_cache_gc::GateCacheReclaim::Gone) => {}
                // Already renamed aside, so no gate will use it again; the
                // remainder is a dead rotation the next plan proposes.
                Ok(crate::gate_cache_gc::GateCacheReclaim::Interrupted) => {
                    journal.remaining_gate_caches.remove(0);
                    write_journal(&journal_path, &journal)?;
                    break;
                }
                Err(reason) => failures.push(reason),
            }
            journal.remaining_gate_caches.remove(0);
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

        while !journal.remaining_recovery_archives.is_empty() && !check_deadline(deadline) {
            let candidate = journal.remaining_recovery_archives[0].clone();
            let dir = PathBuf::from(&candidate.path);
            if std::fs::symlink_metadata(&dir).is_err() {
                journal.remaining_recovery_archives.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            }
            // Re-proved rather than remembered: ownership, the head the plan
            // named, and the age-or-landed rule, all against the journal's
            // policy and the time of removal.
            let evidence = self.owned_recovery_archive(&dir).and_then(|archive| {
                if archive.session_id != candidate.session_id || archive.head != candidate.head {
                    return Err("names a different session or head than the reviewed plan".into());
                }
                recovery_archive_disposition(self, &archive, &journal.policy, now_ms())
                    .ok_or_else(|| "is no longer old enough and has not landed".to_string())
            });
            if let Err(reason) = evidence {
                failures.push(format!(
                    "{}: {reason}; review a new GC plan",
                    candidate.path
                ));
                journal.remaining_recovery_archives.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            }
            // The manifest is the ownership evidence, so it goes last.
            match remove_condemned_tree(&dir, Some(crate::cleanup_resolve::MANIFEST_FILE), deadline)
            {
                Ok(TreeRemoval::Complete) => {}
                Ok(TreeRemoval::Interrupted) => break,
                Err(error) => {
                    failures.push(format!("{}: {error}", candidate.path));
                    journal.remaining_recovery_archives.remove(0);
                    write_journal(&journal_path, &journal)?;
                    continue;
                }
            }
            let payload = serde_json::json!({
                "path": candidate.path,
                "session_id": candidate.session_id,
                "head": candidate.head,
                "landed": candidate.landed,
                "bytes": candidate.estimated_bytes,
            })
            .to_string();
            self.store().append_event(
                crate::events::BROKER_GC_RECOVERY_ARCHIVE_REMOVED,
                Some(candidate.session_id),
                Some(&payload),
            )?;
            journal.reclaimed_bytes = journal
                .reclaimed_bytes
                .saturating_add(candidate.estimated_bytes);
            journal.recovery_archives_removed.push(candidate.path);
            journal.remaining_recovery_archives.remove(0);
            write_journal(&journal_path, &journal)?;
        }

        let deadline_reached = check_deadline(deadline)
            && (!journal.remaining_checkpoint_pin_releases.is_empty()
                || !journal.remaining_publication_exposure_expiries.is_empty()
                || !journal.remaining_rows.is_empty()
                || !journal.remaining_files.is_empty()
                || !journal.remaining_worktrees.is_empty()
                || !journal.remaining_artifacts.is_empty()
                || !journal.remaining_gate_caches.is_empty()
                || !journal.remaining_orphans.is_empty()
                || !journal.remaining_recovery_archives.is_empty());
        let complete = journal.remaining_rows.is_empty()
            && journal.remaining_checkpoint_pin_releases.is_empty()
            && journal.remaining_publication_exposure_expiries.is_empty()
            && journal.remaining_files.is_empty()
            && journal.remaining_worktrees.is_empty()
            && journal.remaining_artifacts.is_empty()
            && journal.remaining_gate_caches.is_empty()
            && journal.remaining_orphans.is_empty()
            && journal.remaining_recovery_archives.is_empty();
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
            gate_caches_reclaimed: journal.gate_caches_reclaimed.clone(),
            checkpoint_pins_released: journal.checkpoint_pins_released.clone(),
            publication_exposures_expired: journal.publication_exposures_expired.clone(),
            recovery_archives_removed: journal.recovery_archives_removed.clone(),
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
                "gate_caches_reclaimed": report.gate_caches_reclaimed.len(),
                "checkpoint_pins_released": report.checkpoint_pins_released.len(),
                "publication_exposures_expired": report.publication_exposures_expired.len(),
                "recovery_archives_removed": report.recovery_archives_removed.len(),
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
    use crate::disk_headroom::SweepUrgency;

    const DETACHED_FIXTURE_MODE_ENV: &str = "AETHYME_TEST_DETACHED_GC_MODE";
    const DETACHED_FIXTURE_ROOT_ENV: &str = "AETHYME_TEST_DETACHED_GC_ROOT";

    fn detached_fixture_command(root: &Path, mode: &str) -> Command {
        let executable = std::env::current_exe().unwrap();
        assert_ne!(
            executable.file_name().and_then(|name| name.to_str()),
            Some("aethyme"),
            "the subprocess fixture must use the test harness, never the installed broker"
        );
        let mut command = Command::new(executable);
        let host_state =
            std::fs::read_to_string(detached_fixture_marker(root, "host-state")).unwrap();
        let host_cache =
            std::fs::read_to_string(detached_fixture_marker(root, "host-cache")).unwrap();
        command
            .args([
                "--exact",
                "gc::tests::detached_artifact_worker_child_fixture",
                "--nocapture",
            ])
            .env(DETACHED_FIXTURE_MODE_ENV, mode)
            .env(DETACHED_FIXTURE_ROOT_ENV, root)
            .env(ARTIFACT_SWEEP_WORKER_ENV, "1")
            .env("AETHYME_HOST_STATE_DIR", host_state.trim())
            .env("AETHYME_HOST_CACHE_DIR", host_cache.trim())
            .env_remove(crate::BROKER_DB_ENV)
            .env_remove("AETHYME_TEST_DB_SUFFIX")
            .env_remove("AETHYME_WORKTREE_ROOT")
            .env_remove(crate::disk_headroom::TEST_AVAILABLE_BYTES_ENV)
            .env_remove(crate::disk_headroom::TEST_AVAILABLE_INODES_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    #[test]
    fn incomplete_worker_keeps_request_and_a_new_pressure_request_retries_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        let mut broker = Broker::open(root).unwrap();
        broker
            .store()
            .meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "7")
            .unwrap();
        broker
            .store()
            .meta_set(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "3")
            .unwrap();
        let policy = load_retention_policy(root).unwrap();

        broker
            .run_artifact_sweep_recovery_worker_loop(&policy, 7, None, |_broker, _policy| {
                Ok((
                    ArtifactSweepOutcome {
                        directories_reclaimed: 0,
                        complete: false,
                        auto_cleanup: None,
                    },
                    0,
                ))
            })
            .unwrap();
        assert_eq!(
            broker
                .store()
                .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("7"),
            "a stalled cache removal leaves its durable request pending"
        );
        assert_eq!(
            broker
                .store()
                .meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("3")
        );
        assert!(worker_retry_pending(broker.store(), now_ms()).unwrap());

        broker.persist_artifact_sweep_recovery_request().unwrap();
        let retry_generation = broker
            .store()
            .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
            .unwrap()
            .unwrap();
        assert!(retry_generation.parse::<i64>().unwrap() > 7);
        assert!(!worker_retry_pending(broker.store(), now_ms()).unwrap());
        broker
            .run_artifact_sweep_recovery_worker_loop(
                &policy,
                retry_generation.parse().unwrap(),
                None,
                |_broker, _policy| {
                    Ok((
                        ArtifactSweepOutcome {
                            directories_reclaimed: 1,
                            complete: true,
                            auto_cleanup: None,
                        },
                        1,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            broker
                .store()
                .meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some(retry_generation.as_str()),
            "a subsequent eligible pass records the requested generation"
        );
        assert!(!worker_retry_pending(broker.store(), now_ms()).unwrap());
    }

    fn detached_fixture_store(root: &Path) -> crate::BrokerStore {
        crate::BrokerStore::open(&root.join(".aethyme/broker.db")).unwrap()
    }

    #[test]
    fn test_harness_executable_is_never_selected_as_the_detached_runtime() {
        assert!(
            artifact_worker_binaries().is_none(),
            "Cargo test executables must never be launched as installed workers"
        );
    }

    #[test]
    fn incomplete_worker_keeps_request_and_a_new_pressure_request_retries_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        let mut broker = Broker::open(root).unwrap();
        broker
            .store()
            .meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "7")
            .unwrap();
        broker
            .store()
            .meta_set(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "3")
            .unwrap();
        let policy = load_retention_policy(root).unwrap();

        broker
            .run_artifact_sweep_recovery_worker_loop(&policy, 7, None, |_broker, _policy| {
                Ok((
                    ArtifactSweepOutcome {
                        directories_reclaimed: 0,
                        complete: false,
                        auto_cleanup: None,
                    },
                    0,
                ))
            })
            .unwrap();
        assert_eq!(
            broker
                .store()
                .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("7"),
            "a stalled cache removal leaves its durable request pending"
        );
        assert_eq!(
            broker
                .store()
                .meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("3")
        );
        assert!(worker_retry_pending(broker.store(), now_ms()).unwrap());

        broker.persist_artifact_sweep_recovery_request().unwrap();
        let retry_generation = broker
            .store()
            .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
            .unwrap()
            .unwrap();
        assert!(retry_generation.parse::<i64>().unwrap() > 7);
        assert!(!worker_retry_pending(broker.store(), now_ms()).unwrap());
        broker
            .run_artifact_sweep_recovery_worker_loop(
                &policy,
                retry_generation.parse().unwrap(),
                None,
                |_broker, _policy| {
                    Ok((
                        ArtifactSweepOutcome {
                            directories_reclaimed: 1,
                            complete: true,
                            auto_cleanup: None,
                        },
                        1,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            broker
                .store()
                .meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some(retry_generation.as_str()),
            "a subsequent eligible pass records the requested generation"
        );
        assert!(!worker_retry_pending(broker.store(), now_ms()).unwrap());
    }

    fn detached_fixture_marker(root: &Path, name: &str) -> PathBuf {
        root.join(".aethyme/test-worker").join(name)
    }

    fn wait_for_fixture_marker(path: &Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if path.is_file() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        path.is_file()
    }

    #[cfg(unix)]
    fn fixture_process_is_alive(pid: u32) -> bool {
        // SAFETY: signal 0 only probes whether the supplied process exists.
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn git(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// On a repository with hundreds of retained worktrees the build-output
    /// walk ran for minutes inside `doctor` (#460). A health check stops at
    /// its budget and counts what it did not scan; a complete scan still
    /// finds the cache.
    #[test]
    fn a_health_check_counts_worktrees_it_had_no_budget_to_scan() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
        git(tmp.path(), &["add", "-A"]);
        git(tmp.path(), &["commit", "-qm", "init"]);
        std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
        std::fs::write(
            tmp.path().join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 30\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join(".git/info/exclude"), "target/\n").unwrap();
        let mut broker = Broker::open(tmp.path()).unwrap();
        let session = broker.start_worktree("retained build", None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
        git(&worktree, &["add", "done.txt"]);
        git(&worktree, &["commit", "-qm", "done"]);
        assert!(broker.submit(session.id).unwrap().promoted);
        assert!(
            broker
                .finish_with_options(
                    session.id,
                    crate::FinishOptions {
                        keep_worktree: true,
                    },
                )
                .unwrap()
                .closed
        );
        let cache = worktree.join("target");
        std::fs::create_dir_all(cache.join("debug")).unwrap();
        std::fs::write(cache.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();

        let policy = load_retention_policy(broker.main_root()).unwrap();
        let cleanup = broker.cleanup_plan_recorded().unwrap();
        let sessions = broker
            .store()
            .cleaned_sessions()
            .unwrap()
            .into_iter()
            .map(|session| (session.id, session))
            .collect::<BTreeMap<_, _>>();
        let scan = |budget| {
            broker.artifact_candidates_within(
                now_ms(),
                &policy,
                &cleanup.worktrees,
                &sessions,
                ArtifactExclusions {
                    already_removed: &[],
                    live_worktrees: &[],
                },
                crate::SizeScan::Recorded,
                budget,
            )
        };

        let complete = scan(None);
        assert_eq!(complete.not_scanned, 0);
        assert_eq!(complete.candidates.len(), 1);
        assert_eq!(complete.candidates[0].relative_dir, "target");
        assert!(
            complete.declined.is_empty(),
            "a recorded pass reports no declined directories"
        );

        let bounded = scan(Some(std::time::Duration::ZERO));
        assert_eq!(bounded.not_scanned, 1);
        assert!(bounded.candidates.is_empty());
    }

    /// A broker open runs before every command and every hook call, so its
    /// share of the sweep is small and fixed. Disk pressure used to multiply
    /// it to a minute per command.
    #[test]
    fn a_broker_open_never_spends_more_than_the_inline_cap_even_under_pressure() {
        for urgency in [SweepUrgency::Routine, SweepUrgency::Pressured] {
            assert_eq!(
                artifact_sweep_budget_ms(5_000, ArtifactSweepScope::Inline, urgency),
                INLINE_ARTIFACT_SWEEP_BUDGET_MS,
                "{urgency:?}"
            );
            assert_eq!(
                artifact_sweep_budget_ms(60_000, ArtifactSweepScope::Inline, urgency),
                INLINE_ARTIFACT_SWEEP_BUDGET_MS,
                "{urgency:?}"
            );
        }
        // A policy budget below the cap is honoured as given.
        assert_eq!(
            artifact_sweep_budget_ms(100, ArtifactSweepScope::Inline, SweepUrgency::Pressured),
            100
        );
        assert!(!scope_runs_checkout_cleanup(
            ArtifactSweepScope::AutomaticContinuation
        ));
        assert!(scope_runs_checkout_cleanup(ArtifactSweepScope::Drain));
        assert!(!scope_runs_checkout_cleanup(
            ArtifactSweepScope::ClosedSession(1)
        ));
    }

    #[test]
    fn interrupted_tree_reports_entries_unlinked_before_its_cursor_stalls() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("target");
        std::fs::create_dir_all(cache.join("nested")).unwrap();
        std::fs::write(cache.join("nested/first"), b"first").unwrap();
        std::fs::write(cache.join("nested/second"), b"second").unwrap();

        let (outcome, entries_reclaimed) =
            remove_condemned_tree_with_progress(&cache, None, Some(Instant::now())).unwrap();

        assert_eq!(outcome, TreeRemoval::Interrupted);
        assert!(
            entries_reclaimed > 0,
            "partial unlink progress must be visible even when the worker resumes the same tree"
        );
        assert!(cache.exists(), "the interrupted cache remains resumable");
    }

    #[test]
    fn an_automatic_recovery_request_advances_its_durable_generation() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
        git(tmp.path(), &["add", "README.md"]);
        git(tmp.path(), &["commit", "-qm", "init"]);
        let mut broker = Broker::open(tmp.path()).unwrap();

        assert!(!broker.artifact_sweep_recovery_requested().unwrap());
        broker.request_artifact_sweep_recovery().unwrap();
        assert!(broker.artifact_sweep_recovery_requested().unwrap());
        let first = broker
            .store()
            .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
            .unwrap();
        broker.request_artifact_sweep_recovery().unwrap();
        let second = broker
            .store()
            .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
            .unwrap();
        let first_generation = first.unwrap().parse::<i64>().unwrap();
        let second_generation = second.unwrap().parse::<i64>().unwrap();
        assert!(
            second_generation > first_generation,
            "a later pressure request must not be erased by a worker finishing an older lap"
        );
    }

    /// This test-harness fixture is invoked in child processes by
    /// `detached_worker_survives_its_launcher_and_resumes_after_a_crash`.
    /// It deliberately exercises the real advisory lock, metadata store, and
    /// cache-tree remover without resolving or launching an installed broker.
    #[test]
    fn detached_artifact_worker_child_fixture() {
        let Ok(mode) = std::env::var(DETACHED_FIXTURE_MODE_ENV) else {
            return;
        };
        let root = PathBuf::from(std::env::var_os(DETACHED_FIXTURE_ROOT_ENV).unwrap());
        match mode.as_str() {
            "setup" => {
                let worktree_root =
                    std::fs::read_to_string(detached_fixture_marker(&root, "worktree-root"))
                        .unwrap();
                let mut broker = Broker::open(&root)
                    .unwrap()
                    .with_worktree_root(worktree_root.trim());
                let closed = broker.start_worktree("closed cache fixture", None).unwrap();
                let closed_worktree = PathBuf::from(&closed.worktree_path);
                std::fs::write(closed_worktree.join("landed.txt"), "landed\n").unwrap();
                git(&closed_worktree, &["add", "landed.txt"]);
                git(&closed_worktree, &["commit", "-qm", "landed"]);
                assert!(broker.submit(closed.id).unwrap().promoted);
                assert!(
                    broker
                        .finish_with_options(
                            closed.id,
                            crate::FinishOptions {
                                keep_worktree: true,
                            },
                        )
                        .unwrap()
                        .closed
                );

                let active = broker.start_worktree("active cache fixture", None).unwrap();
                let active_worktree = PathBuf::from(&active.worktree_path);
                let closed_cache = closed_worktree.join("target");
                std::fs::create_dir_all(closed_cache.join("debug")).unwrap();
                std::fs::write(
                    closed_cache.join("CACHEDIR.TAG"),
                    "Signature: 8a477f597d28d172\n",
                )
                .unwrap();
                for index in 0..128 {
                    std::fs::write(
                        closed_cache.join("debug").join(format!("entry-{index:03}")),
                        vec![b'x'; 4096],
                    )
                    .unwrap();
                }
                let active_cache = active_worktree.join("target");
                std::fs::create_dir_all(active_cache.join("debug")).unwrap();
                std::fs::write(
                    active_cache.join("CACHEDIR.TAG"),
                    "Signature: 8a477f597d28d172\n",
                )
                .unwrap();
                std::fs::write(active_cache.join("debug/keep.txt"), "live cache\n").unwrap();
                for (name, value) in [
                    ("closed-worktree", closed.worktree_path.as_str()),
                    ("active-worktree", active.worktree_path.as_str()),
                    ("closed-branch", closed.branch.as_str()),
                ] {
                    std::fs::write(detached_fixture_marker(&root, name), value).unwrap();
                }
                std::fs::write(
                    detached_fixture_marker(&root, "closed-session-id"),
                    closed.id.to_string(),
                )
                .unwrap();
                drop(broker);
            }
            "contend" => {
                assert!(
                    try_artifact_sweep_worker_lock(&root).unwrap().is_none(),
                    "a second process must not enter while the singleton lock is held"
                );
                std::fs::write(
                    detached_fixture_marker(&root, "contender-blocked"),
                    b"blocked",
                )
                .unwrap();
            }
            "launch-crash-child" => {
                let mut child = detached_fixture_command(&root, "crash-child")
                    .spawn()
                    .unwrap();
                std::fs::write(
                    detached_fixture_marker(&root, "child-pid"),
                    child.id().to_string(),
                )
                .unwrap();
                if !wait_for_fixture_marker(
                    &detached_fixture_marker(&root, "child-ready"),
                    Duration::from_secs(10),
                ) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("detached child did not acquire its worker lock");
                }
                // Dropping Child does not wait for it. Returning from this
                // test exits the launcher while its detached child is held
                // behind the parent's explicit exit marker.
            }
            "crash-child" => {
                let lock = acquire_artifact_sweep_worker_lock(&root).unwrap();
                let mut broker = Broker::open(&root).unwrap();
                std::fs::write(detached_fixture_marker(&root, "child-ready"), b"ready").unwrap();
                assert!(wait_for_fixture_marker(
                    &detached_fixture_marker(&root, "parent-exited"),
                    Duration::from_secs(20),
                ));

                let closed_worktree = PathBuf::from(
                    std::fs::read_to_string(detached_fixture_marker(&root, "closed-worktree"))
                        .unwrap(),
                );
                let active_worktree = PathBuf::from(
                    std::fs::read_to_string(detached_fixture_marker(&root, "active-worktree"))
                        .unwrap(),
                );
                let closed_session_id =
                    std::fs::read_to_string(detached_fixture_marker(&root, "closed-session-id"))
                        .unwrap()
                        .trim()
                        .parse::<i64>()
                        .unwrap();
                let cache = closed_worktree.join("target");
                let active_cache = active_worktree.join("target");
                assert!(cache.join("debug").is_dir());
                assert!(active_cache.join("debug/keep.txt").is_file());
                let session = broker.store().session(closed_session_id).unwrap();
                assert!(
                    broker
                        .store()
                        .cleaned_sessions()
                        .unwrap()
                        .iter()
                        .any(|closed| closed.id == closed_session_id)
                );
                assert!(broker.is_broker_owned_worktree(&session, &closed_worktree));
                let policy = load_retention_policy(broker.main_root()).unwrap();
                let mut entries_reclaimed = 0;
                broker
                    .run_artifact_sweep_recovery_worker_loop(
                        &policy,
                        77,
                        Some(1),
                        |broker, policy| {
                            let result = broker.sweep_artifacts_autonomously_with_deadline(
                                policy,
                                ArtifactSweepScope::AutomaticContinuation,
                                Some(Instant::now()),
                            );
                            if let Ok((_, count)) = &result {
                                entries_reclaimed = *count;
                            }
                            result
                        },
                    )
                    .unwrap();
                assert!(
                    entries_reclaimed > 0,
                    "the detached cache-only child must make filesystem progress"
                );
                assert_eq!(
                    broker
                        .store()
                        .meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                        .unwrap()
                        .as_deref(),
                    Some("0"),
                    "an incomplete worker pass must not advance the completed generation"
                );
                assert!(
                    cache.is_dir(),
                    "partial cache deletion must remain resumable"
                );
                assert!(active_cache.is_dir(), "a live session cache is protected");

                std::fs::write(
                    detached_fixture_marker(&root, "child-progress"),
                    entries_reclaimed.to_string(),
                )
                .unwrap();
                drop(broker);
                drop(lock);
                // Model a process crash after durable progress was recorded.
                // The request and cursor intentionally remain outstanding.
                std::process::exit(86);
            }
            "resume-child" => {
                let lock = acquire_artifact_sweep_worker_lock(&root).unwrap();
                let mut broker = Broker::open(&root).unwrap();
                let closed_worktree = PathBuf::from(
                    std::fs::read_to_string(detached_fixture_marker(&root, "closed-worktree"))
                        .unwrap(),
                );
                let active_worktree = PathBuf::from(
                    std::fs::read_to_string(detached_fixture_marker(&root, "active-worktree"))
                        .unwrap(),
                );
                let closed_session_id =
                    std::fs::read_to_string(detached_fixture_marker(&root, "closed-session-id"))
                        .unwrap();
                let cache = closed_worktree.join("target");
                let active_cache = active_worktree.join("target");
                {
                    let store = broker.store();
                    let requested = store
                        .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                        .unwrap()
                        .unwrap();
                    assert_eq!(requested, "77");
                    assert_eq!(
                        store
                            .meta_get(ARTIFACT_SWEEP_CURSOR_KEY)
                            .unwrap()
                            .as_deref(),
                        Some(closed_session_id.trim()),
                        "the restart must see the saved session cursor"
                    );
                    assert_eq!(
                        store
                            .meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                            .unwrap()
                            .as_deref(),
                        Some(cache.to_string_lossy().as_ref()),
                        "the restart must see the interrupted cache path"
                    );
                }

                let policy = load_retention_policy(broker.main_root()).unwrap();
                let mut entries_reclaimed = 0;
                broker
                    .run_artifact_sweep_recovery_worker_loop(&policy, 77, None, |broker, policy| {
                        let result = broker.sweep_artifacts_autonomously_with_progress(
                            policy,
                            ArtifactSweepScope::AutomaticContinuation,
                        );
                        if let Ok((_, count)) = &result {
                            entries_reclaimed = entries_reclaimed.saturating_add(*count);
                        }
                        result
                    })
                    .unwrap();
                assert!(entries_reclaimed > 0);
                assert!(!cache.exists(), "the closed session cache is reclaimed");
                assert!(active_cache.join("debug/keep.txt").is_file());
                assert!(closed_worktree.is_dir(), "the retained checkout survives");
                assert!(GitRepo::discover(&closed_worktree).is_ok());
                let branch =
                    std::fs::read_to_string(detached_fixture_marker(&root, "closed-branch"))
                        .unwrap();
                assert!(
                    std::process::Command::new("git")
                        .args([
                            "show-ref",
                            "--verify",
                            &format!("refs/heads/{}", branch.trim()),
                        ])
                        .current_dir(&root)
                        .status()
                        .unwrap()
                        .success(),
                    "cache-only continuation must preserve the closed session branch"
                );
                let requested = broker
                    .store()
                    .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    broker
                        .store()
                        .meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                        .unwrap()
                        .as_deref(),
                    Some(requested.as_str()),
                    "the worker loop advances the generation after a complete lap"
                );
                std::fs::write(
                    detached_fixture_marker(&root, "child-resumed"),
                    entries_reclaimed.to_string(),
                )
                .unwrap();
                drop(broker);
                drop(lock);
            }
            other => panic!("unknown detached worker fixture mode {other:?}"),
        }
    }

    #[test]
    fn detached_worker_survives_its_launcher_and_resumes_after_a_crash() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
        git(root, &["add", "README.md", ".gitignore"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 0\nclosed_worktree_grace_hours = 0\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\nartifact_sweep_interval_hours = 24\nidle_session_artifact_hours = 0\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".aethyme/test-worker")).unwrap();

        let worktree_root = tempfile::tempdir().unwrap();
        let host_state = tempfile::tempdir().unwrap();
        let host_cache = tempfile::tempdir().unwrap();
        for (name, path) in [
            ("worktree-root", worktree_root.path()),
            ("host-state", host_state.path()),
            ("host-cache", host_cache.path()),
        ] {
            std::fs::write(
                detached_fixture_marker(root, name),
                path.to_string_lossy().as_ref(),
            )
            .unwrap();
        }
        let setup = detached_fixture_command(root, "setup").status().unwrap();
        assert!(setup.success());
        let closed_worktree = PathBuf::from(
            std::fs::read_to_string(detached_fixture_marker(root, "closed-worktree")).unwrap(),
        );
        let active_worktree = PathBuf::from(
            std::fs::read_to_string(detached_fixture_marker(root, "active-worktree")).unwrap(),
        );
        let closed_cache = closed_worktree.join("target");
        let active_cache = active_worktree.join("target");

        {
            let store = detached_fixture_store(root);
            store
                .meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "77")
                .unwrap();
            store
                .meta_set(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "0")
                .unwrap();
            store.meta_set(ARTIFACT_SWEEP_CURSOR_KEY, "0").unwrap();
            store.meta_set(ARTIFACT_SWEEP_PROGRESS_KEY, "1").unwrap();
            store.meta_set(ARTIFACT_SWEEP_INTERRUPTED_KEY, "").unwrap();
        }
        let cache_entries_before = std::fs::read_dir(closed_cache.join("debug"))
            .unwrap()
            .count();

        // Hold the OS lock in this process and prove a separate test-binary
        // process cannot enter its cache-only section.
        let held_lock = acquire_artifact_sweep_worker_lock(root).unwrap();
        let contender = detached_fixture_command(root, "contend").status().unwrap();
        assert!(contender.success());
        assert_eq!(
            std::fs::read(detached_fixture_marker(root, "contender-blocked")).unwrap(),
            b"blocked"
        );
        assert_eq!(
            std::fs::read_dir(closed_cache.join("debug"))
                .unwrap()
                .count(),
            cache_entries_before
        );
        drop(held_lock);

        // A launcher process starts the worker, waits for its ready signal,
        // then exits before the test permits the worker to do any work.
        let launcher = detached_fixture_command(root, "launch-crash-child")
            .status()
            .unwrap();
        assert!(launcher.success());
        std::fs::write(detached_fixture_marker(root, "parent-exited"), b"exited").unwrap();
        assert!(wait_for_fixture_marker(
            &detached_fixture_marker(root, "child-progress"),
            Duration::from_secs(10),
        ));
        assert!(wait_for_fixture_marker(
            &detached_fixture_marker(root, "child-pid"),
            Duration::from_secs(1),
        ));
        #[cfg(unix)]
        let child_pid = std::fs::read_to_string(detached_fixture_marker(root, "child-pid"))
            .unwrap()
            .parse::<u32>()
            .unwrap();

        // The crash marker is only written after SQLite progress is durable;
        // wait for the child's advisory lock to be released before restart.
        let lock_deadline = Instant::now() + Duration::from_secs(10);
        let released_lock = loop {
            if Instant::now() >= lock_deadline {
                panic!("crashed worker did not release its singleton lock");
            }
            match try_artifact_sweep_worker_lock(root).unwrap() {
                Some(lock) => break lock,
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        drop(released_lock);
        #[cfg(unix)]
        {
            let exit_deadline = Instant::now() + Duration::from_secs(5);
            while fixture_process_is_alive(child_pid) && Instant::now() < exit_deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(
                !fixture_process_is_alive(child_pid),
                "the interrupted worker must exit instead of remaining detached forever"
            );
        }
        let partially_remaining = std::fs::read_dir(closed_cache.join("debug"))
            .unwrap()
            .count();
        assert!(partially_remaining < cache_entries_before && partially_remaining > 0);
        assert!(active_cache.join("debug/keep.txt").is_file());
        {
            let store = detached_fixture_store(root);
            assert_eq!(
                store
                    .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                    .unwrap()
                    .as_deref(),
                Some("77")
            );
            assert_eq!(
                store
                    .meta_get(ARTIFACT_SWEEP_CURSOR_KEY)
                    .unwrap()
                    .as_deref(),
                Some(
                    std::fs::read_to_string(detached_fixture_marker(root, "closed-session-id",))
                        .unwrap()
                        .trim()
                )
            );
            assert_eq!(
                store
                    .meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                    .unwrap()
                    .as_deref(),
                Some(closed_cache.to_string_lossy().as_ref())
            );
        }

        let resumed = detached_fixture_command(root, "resume-child")
            .status()
            .unwrap();
        assert!(resumed.success());
        assert!(detached_fixture_marker(root, "child-resumed").is_file());
        assert!(!closed_cache.exists());
        assert!(active_cache.join("debug/keep.txt").is_file());
        assert!(closed_worktree.is_dir());
        assert!(GitRepo::discover(&closed_worktree).is_ok());
        assert!(root.join("README.md").is_file());
        assert!(root.join(".git/HEAD").is_file());
        let store = detached_fixture_store(root);
        assert_eq!(
            store
                .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("77"),
            "the request is durable history; completion advances separately"
        );
        assert_eq!(
            store
                .meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("77")
        );
        assert_eq!(
            store
                .meta_get(ARTIFACT_SWEEP_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some("0")
        );
        assert_eq!(
            store
                .meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                .unwrap()
                .as_deref(),
            Some("")
        );
    }

    /// Each case is rejected by exactly one guard, so each guard is proved on
    /// its own rather than masked by the others.
    #[test]
    fn a_remembered_tree_is_resumed_only_when_every_guard_holds() {
        let root_dir = tempfile::tempdir().unwrap();
        let root = root_dir.path();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.join(".pnpm-store")).unwrap();
        std::fs::create_dir_all(root.join("web/node_modules")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(outside.path().join("node_modules")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
        std::fs::create_dir_all(root.join("real/node_modules")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("inner-link")).unwrap();

        assert!(resumable_interrupted_tree(
            root,
            &root.join(".pnpm-store"),
            &[]
        ));
        assert!(resumable_interrupted_tree(
            root,
            &root.join("web/node_modules"),
            &[]
        ));
        // Component guard only: canonically inside root, no symlink, an
        // artefact name -- but it is spelled with `..`.
        assert!(!resumable_interrupted_tree(
            root,
            &root.join("a/../.pnpm-store"),
            &[]
        ));
        // Name guard only: a real directory inside root, but not a cache.
        assert!(!resumable_interrupted_tree(root, &root.join("src"), &[]));
        assert!(!resumable_interrupted_tree(
            root,
            &root.join("src"),
            &["src".to_string()]
        ));
        std::fs::create_dir(root.join("custom-cache")).unwrap();
        assert!(resumable_interrupted_tree(
            root,
            &root.join("custom-cache"),
            &["custom-cache".to_string()]
        ));
        // Symlink guard only: the link resolves inside root.
        assert!(!resumable_interrupted_tree(
            root,
            &root.join("inner-link/node_modules"),
            &[]
        ));
        // A link leaving root.
        assert!(!resumable_interrupted_tree(
            root,
            &root.join("link/node_modules"),
            &[]
        ));
        // Not under root at all, and root itself.
        assert!(!resumable_interrupted_tree(
            root,
            &outside.path().join("node_modules"),
            &[]
        ));
        assert!(!resumable_interrupted_tree(root, root, &[]));
        // Gone.
        assert!(!resumable_interrupted_tree(
            root,
            &root.join("node_modules"),
            &[]
        ));
    }

    /// Explicit reclaim commands keep the pressure-scaled policy budget.
    #[test]
    fn explicit_and_closed_session_sweeps_keep_the_pressure_scaled_budget() {
        for scope in [
            ArtifactSweepScope::Drain,
            ArtifactSweepScope::AutomaticContinuation,
            ArtifactSweepScope::ClosedSession(7),
        ] {
            assert_eq!(
                artifact_sweep_budget_ms(5_000, scope, SweepUrgency::Routine),
                5_000
            );
            assert_eq!(
                artifact_sweep_budget_ms(5_000, scope, SweepUrgency::Pressured),
                60_000
            );
        }
    }

    #[test]
    fn legacy_gc_journal_defaults_new_protection_work() {
        let mut policy = serde_json::to_value(RetentionPolicy::default()).unwrap();
        policy
            .as_object_mut()
            .unwrap()
            .remove("publication_exposure_days");
        let journal = serde_json::json!({
            "schema_version": GC_PLAN_SCHEMA_VERSION,
            "digest": "a".repeat(64),
            "evaluated_at": 1,
            "policy": policy,
            "remaining_rows": [],
            "remaining_files": [],
            "remaining_worktrees": [],
            "rows_removed": 0,
            "files_completed": [],
            "sessions_cleaned": [],
            "reclaimed_bytes": 0
        });
        let parsed: GcJournal = serde_json::from_value(journal).unwrap();
        assert_eq!(
            parsed.policy.publication_exposure_days,
            RetentionPolicy::default().publication_exposure_days
        );
        assert!(parsed.remaining_checkpoint_pin_releases.is_empty());
        assert!(parsed.remaining_publication_exposure_expiries.is_empty());
    }

    /// One interrupted pass must advance the next one, and one uninterrupted
    /// pass must still be a full lap whatever the cursor was.
    #[test]
    fn a_sweep_resumes_after_the_cursor_and_still_wraps_to_the_head() {
        let sessions = [1_i64, 2, 3, 4, 5];

        // A fresh cursor visits everything in order.
        assert_eq!(sweep_order(&sessions, 0, |id| *id), vec![1, 2, 3, 4, 5]);

        // After stopping at 3, the next pass starts at 4 -- and still covers
        // 1..=3 afterwards, so nothing is starved.
        assert_eq!(sweep_order(&sessions, 3, |id| *id), vec![4, 5, 1, 2, 3]);

        // A cursor past the end degenerates to a normal full lap rather than
        // an empty pass.
        assert_eq!(sweep_order(&sessions, 99, |id| *id), vec![1, 2, 3, 4, 5]);

        assert!(sweep_order(&[] as &[i64], 7, |id| *id).is_empty());
    }

    fn dir(root: &Path, relative: &str) -> PathBuf {
        let path = root.join(relative);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// (c) for a reused directory: a finished session's row can point at a
    /// checkout a new live session has since adopted. Its build output is the
    /// live session's then, however old the finished row is.
    #[test]
    fn a_worktree_a_live_session_works_in_is_never_a_build_cache_source() {
        let tmp = tempfile::tempdir().unwrap();
        let finished = dir(tmp.path(), "worktrees/finished");
        let nested_live = dir(tmp.path(), "worktrees/finished/nested");
        let sibling = dir(tmp.path(), "worktrees/other");
        assert!(overlaps_live_worktree(
            &finished,
            std::slice::from_ref(&finished)
        ));
        assert!(overlaps_live_worktree(&finished, &[nested_live]));
        assert!(!overlaps_live_worktree(&finished, &[sibling]));
        // A session adopted at an enclosing checkout does not shelter the
        // separate checkouts beneath it.
        assert!(!overlaps_live_worktree(
            &finished,
            &[tmp.path().to_path_buf()]
        ));
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
    fn python_environments_and_pnpm_stores_are_witnessed_build_caches() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let venv = dir(root, "backend/.venv");
        std::fs::write(venv.join("notes.txt"), "not a venv\n").unwrap();
        assert!(artifact_witness_for(&venv).is_none());
        std::fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        assert!(artifact_witness_for(&venv).is_some());

        let store = dir(root, ".pnpm-store");
        assert!(artifact_witness_for(&store).is_none());
        dir(root, ".pnpm-store/v10");
        assert!(artifact_witness_for(&store).is_some());
    }

    /// `build` and `dist` are in the shared catalog, but only reviewed paths
    /// act on them: a non-empty directory is no evidence of build output.
    #[test]
    fn reviewed_only_names_never_classify_for_the_unattended_lane() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["build", "dist"] {
            let path = dir(tmp.path(), name);
            std::fs::write(path.join("index.js"), "x\n").unwrap();
            assert!(artifact_witness_for(&path).is_none(), "{name}");
            assert!(!is_known_artifact_name(name, &[]), "{name}");
        }
    }

    #[test]
    fn configured_source_and_control_names_are_never_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let source = dir(tmp.path(), "src");
        std::fs::write(source.join("main.rs"), "fn main() {}\n").unwrap();

        assert!(
            artifact_witness_for_with_extras(&source, &["src".into()]).is_none(),
            "configured source roots must not become deletion candidates"
        );
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
