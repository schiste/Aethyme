//! Pure broker garbage-collection planning.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};

use crate::artifact_catalog::ArtifactWitness;
#[cfg(test)]
use crate::broker::WORKTREE_ROOT_MARKER;
use crate::broker::directory_size_without_following_links;
use crate::{
    Broker, BrokerOpError, GcApplyReport, GcArtifactCandidate, GcBlocker, GcBlockerSummary,
    GcCheckpointPinRelease, GcDeclinedArtifact, GcFileAction, GcFileCandidate,
    GcGateCacheCandidate, GcHealth, GcOrphanCandidate, GcPlan, GcPublicationExposureExpiry,
    GcRecoveryArchiveCandidate, GcRowCandidate, GcWorktreeBlockerSummary, GcWorktreeCandidate,
    GitRepo, OperationStatus, RecoveryArchiveInventory, RetentionPolicy,
    SessionRetirementDeferredReason, load_retention_policy, load_retention_policy_report,
};

mod tree_removal;
pub(crate) use tree_removal::{
    PinnedTreeRemoval, TreeObjectIdentity, TreeRemoval, TreeRemovalError, remove_condemned_tree,
    remove_condemned_tree_with_progress_and_guard,
};

pub const GC_PLAN_SCHEMA_VERSION: u32 = 4;
const GC_LEGACY_PLAN_SCHEMA_VERSION: u32 = 3;
const MILLIS_PER_DAY: i64 = 86_400_000;
const GC_JOURNAL_ARCHIVE_DIRECTORY: &str = "gc-journal-archives";
const GC_JOURNAL_ARCHIVE_PREFIX: &str = "gc-journal-abandoned-";
const GC_JOURNAL_ARCHIVE_SUFFIX: &str = ".json";
const GC_JOURNAL_ARCHIVE_MAX_COUNT: usize = 32;
const GC_JOURNAL_ARCHIVE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const GC_JOURNAL_ARCHIVE_MAX_DIRECTORY_ENTRIES: usize = 64;

fn gc_journal_archive_directory_scan_has_budget(entries_seen: usize) -> bool {
    entries_seen <= GC_JOURNAL_ARCHIVE_MAX_DIRECTORY_ENTRIES
}

fn gc_journal_schema_is_revocable(version: u32) -> bool {
    matches!(
        version,
        2 | GC_LEGACY_PLAN_SCHEMA_VERSION | GC_PLAN_SCHEMA_VERSION
    )
}

fn gc_journal_schema_is_resumable(version: u32) -> bool {
    matches!(
        version,
        GC_LEGACY_PLAN_SCHEMA_VERSION | GC_PLAN_SCHEMA_VERSION
    )
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn deadline_expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

fn gc_journal_archive_budget_admits(count: usize, bytes: u64, incoming: u64) -> bool {
    count < GC_JOURNAL_ARCHIVE_MAX_COUNT
        && bytes
            .checked_add(incoming)
            .is_some_and(|total| total <= GC_JOURNAL_ARCHIVE_MAX_BYTES)
}

/// How deep below a worktree root a build directory is looked for. Deep enough
/// for nested workspaces and package directories, shallow enough to keep the
/// scan bounded on large trees.
const ARTIFACT_SCAN_DEPTH: usize = 6;
/// Maximum number of directory entries the artifact scanner examines in one
/// worktree pass. The wall-clock budget remains the primary limit; this also
/// bounds broad trees when metadata calls are unusually cheap.
const ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET: usize = 16_384;

/// How long a recorded-size plan spends looking for build directories in
/// retained worktrees before it stops and reports how many it did not scan.
/// `doctor`, `certify` and the verify loop take that path, and on a
/// repository with hundreds of retained worktrees an unbounded walk ran for
/// minutes (#460). The explicit `gc plan --full-scan` path has no such scan
/// budget.
pub const HEALTH_CHECK_ARTIFACT_SCAN_BUDGET: std::time::Duration =
    std::time::Duration::from_secs(10);
/// Default cap for an operator's manual `gc plan` inventory. `--full-scan`
/// retains the exhaustive reviewed plan path for repositories that need it.
pub(crate) const MANUAL_GC_PLAN_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

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
/// never reaches the backlog behind it. An incompletely discovered worktree
/// is recorded as an incomplete pass, but the cursor still advances past it;
/// every later closed session gets a turn and no pass can claim completion
/// while any worktree was unclassified. The cadence stamp records *when* a
/// pass ran; this records *how far* it got (#222).
const ARTIFACT_SWEEP_CURSOR_KEY: &str = "gc.artifact_sweep.cursor_session_id";
/// When an unfinished pass last made progress on a broker open.
///
/// An unfinished pass withholds the cadence stamp so the backlog is not hidden
/// for a whole interval. Without this second stamp that made *every* broker
/// open resume the backlog: a repository whose backlog outlasts one budget
/// paid a full budget -- a minute under disk pressure -- on every `gh`, `note`
/// and hook call. Inline continuation is spaced by this instead.
const ARTIFACT_SWEEP_PROGRESS_KEY: &str = "gc.artifact_sweep.last_partial_ms";

/// Build caches that an unfinished pass was part-way through removing.
///
/// Unlinking a tree's contents updates its modification time, so without
/// this queue the next pass mistook its own partial removal for a writer and
/// protected an idle open session's half-removed cache for another idle
/// window. Legacy single-path values remain readable; multiple in-progress
/// trees are stored as a bounded JSON array.
const ARTIFACT_SWEEP_INTERRUPTED_KEY: &str = "gc.artifact_sweep.interrupted_path";
const ARTIFACT_SWEEP_INTERRUPTED_PATHS_MAX: usize = 32;
const ARTIFACT_SWEEP_INTERRUPTED_VALUE_MAX_BYTES: usize = 64 * 1024;
/// Typed recovery queue for arbitrary ignored directory roots. The legacy
/// interrupted-path queue remains catalog-only for older brokers.
const ARTIFACT_SWEEP_IGNORED_DIR_INTERRUPTED_KEY: &str =
    "gc.artifact_sweep.interrupted_ignored_directory";
const IGNORED_DIRECTORY_QUEUE_SCHEMA_VERSION: u32 = 1;
const ARTIFACT_SWEEP_DIAGNOSTIC_PATH_MAX_CHARS: usize = 128;
const ARTIFACT_SWEEP_BLOCKED_DIAGNOSTICS_MAX: usize = 32;
/// Last diagnosed unavailable marker set; replaced or cleared to avoid an
/// append-only event for every retry of the same blocked paths.
const ARTIFACT_SWEEP_BLOCKED_PATHS_KEY: &str = "gc.artifact_sweep.blocked_paths";
/// Bounded JSON for the capped diagnostic list: each entry has one path capped
/// to 128 characters plus a fixed reason and JSON framing.
const ARTIFACT_SWEEP_BLOCKED_PATHS_VALUE_MAX_BYTES: usize =
    ARTIFACT_SWEEP_BLOCKED_DIAGNOSTICS_MAX * (6 * ARTIFACT_SWEEP_DIAGNOSTIC_PATH_MAX_CHARS + 256);
/// Durable request for the detached, cache-only continuation worker.
const ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY: &str = "gc.artifact_sweep.recovery_requested_ms";
/// Highest recovery request generation completed by the cache-only worker.
const ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY: &str = "gc.artifact_sweep.recovery_completed_ms";
/// Retry time for an automatic continuation that made no filesystem progress.
const ARTIFACT_SWEEP_RETRY_AFTER_KEY: &str = "gc.artifact_sweep.retry_after_ms";
/// Separate durable request for whole-checkout cleanup deferred by an ordinary
/// automatic pass. Cache-only recovery requests never set this marker.
pub(crate) const AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY: &str =
    "gc.artifact_sweep.checkout_cleanup_requested_ms";
/// Highest whole-checkout cleanup request generation completed by its lane.
pub(crate) const AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY: &str =
    "gc.artifact_sweep.checkout_cleanup_completed_ms";
/// Backoff for a whole-checkout cleanup continuation that made no progress.
pub(crate) const AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY: &str =
    "gc.artifact_sweep.checkout_cleanup_retry_after_ms";
/// Separate bounded request for open-session retirement. Its retry is
/// independent of closed-checkout grace and physical-removal cooldowns.
const SESSION_RETIREMENT_STATE_KEY: &str = "gc.artifact_sweep.session_retirement_state";
const SESSION_RETIREMENT_STATE_VERSION: u8 = 1;
const SESSION_RETIREMENT_STATE_CAS_ATTEMPTS: usize = 4;

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct SessionRetirementRequestState {
    version: u8,
    requested_generation: i64,
    completed_generation: i64,
    retry_after_ms: i64,
}

impl Default for SessionRetirementRequestState {
    fn default() -> Self {
        Self {
            version: SESSION_RETIREMENT_STATE_VERSION,
            requested_generation: 0,
            completed_generation: 0,
            retry_after_ms: 0,
        }
    }
}
/// One-time bounded backfill of retained closed Spawned rows when the opt-in
/// compact policy is enabled. This schedules witnessed candidates; the
/// per-session outbox and fresh removal proof remain authoritative.
const AUTO_COMPACT_BACKFILL_REQUESTED_KEY: &str = "gc.artifact_sweep.compact_backfill_requested_ms";
const AUTO_COMPACT_BACKFILL_COMPLETED_KEY: &str = "gc.artifact_sweep.compact_backfill_completed_ms";
const AUTO_COMPACT_BACKFILL_RETRY_AFTER_KEY: &str =
    "gc.artifact_sweep.compact_backfill_retry_after_ms";
const AUTO_COMPACT_BACKFILL_CURSOR_KEY: &str = "gc.artifact_sweep.compact_backfill_cursor";
const AUTO_COMPACT_BACKFILL_HIGH_WATER_KEY: &str = "gc.artifact_sweep.compact_backfill_high_water";
const AUTO_COMPACT_POLICY_ENABLED_KEY: &str = "gc.artifact_sweep.compact_policy_enabled";
/// Bounded last-worker result; overwritten each run rather than appended.
const ARTIFACT_SWEEP_WORKER_STATUS_KEY: &str = "gc.artifact_sweep.worker_status";
const ARTIFACT_SWEEP_META_PREFIX: &str = "gc.artifact_sweep.";
const ARTIFACT_SWEEP_LEGACY_SCALAR_MAX_BYTES: usize = 4 * 1024;
/// Internal marker which keeps Broker::open from starting a second inline GC
/// pass before the continuation worker takes its scoped recovery path.
pub(crate) const ARTIFACT_SWEEP_WORKER_ENV: &str = "AETHYME_GC_ARTIFACT_SWEEP_WORKER";
const ARTIFACT_SWEEP_WORKER_NO_PROGRESS_PASSES: usize = 3;
/// Positive partial deletion is progress, but must not let one detached worker
/// run indefinitely while another process keeps replenishing the cache.
const ARTIFACT_SWEEP_WORKER_MAX_PASSES: usize = 8;
/// Discovery pages count as progress; the wall-time budget still bounds the worker.
const AUTO_CHECKOUT_CLEANUP_WORKER_MAX_PASSES: usize = 4096;
/// Pressure can multiply one sweep budget by twelve. Cap the worker's entire
/// lifetime as well as each individual pass, retaining the cursor/request for
/// a later normal open when this deadline expires.
const ARTIFACT_SWEEP_WORKER_MAX_DURATION: Duration = Duration::from_secs(5 * 60);
const ARTIFACT_SWEEP_WORKER_RETRY_MS: i64 = 10 * 60_000;
/// Cache work yields this part of the singleton worker's total budget to an
/// independently requested whole-checkout cleanup pass.
const AUTO_CHECKOUT_CLEANUP_WORKER_RESERVE: Duration = Duration::from_secs(60);
/// The detached worker gives an opt-in compact archive its own admission
/// window. Foreground sweeps never archive checkout content.
const AUTO_COMPACT_CHECKOUT_WORKER_RESERVE: Duration = Duration::from_secs(90);
/// An archive may start with less than its nominal lane reservation, but it
/// still needs the full cleanup helper minimum after fresh index work.
const AUTO_COMPACT_CHECKOUT_WORKER_ADMISSION_MINIMUM: Duration =
    crate::auto_cleanup::AUTO_COMPACTION_MIN_BUDGET;
const AUTO_COMPACT_CHECKOUT_RETRY_MS: i64 = 10 * 60_000;
const AUTO_COMPACT_BACKFILL_RETRY_MS: i64 = 60_000;
const AUTO_COMPACT_CHECKOUT_BATCH_SIZE: usize = 8;
const AUTO_COMPACT_BACKFILL_PAGE_SIZE: usize = 32;

fn artifact_worker_cache_lane_duration(
    checkout_cleanup_due: bool,
    compact_checkout_due: bool,
    session_retirement_due: bool,
) -> Duration {
    let mut reserve = Duration::ZERO;
    if checkout_cleanup_due {
        reserve = reserve.saturating_add(AUTO_CHECKOUT_CLEANUP_WORKER_RESERVE);
    }
    // Always keep one retirement tranche after the cache lane for requests
    // that arrive while this singleton worker is busy. When retirement is
    // already due, keep a second tranche for the pre-cache pass.
    reserve = reserve.saturating_add(SESSION_RETIREMENT_WORKER_RESERVE);
    if session_retirement_due {
        reserve = reserve.saturating_add(SESSION_RETIREMENT_WORKER_RESERVE);
    }
    if compact_checkout_due {
        reserve = reserve.saturating_add(AUTO_COMPACT_CHECKOUT_WORKER_RESERVE);
    }
    ARTIFACT_SWEEP_WORKER_MAX_DURATION.saturating_sub(reserve)
}

/// Spend one worker admission on several fresh candidate checks, while keeping
/// the full archive reserve before each attempt. `false` leaves later rows due.
fn run_compact_checkout_candidates<T, E>(
    candidates: &[T],
    mut remaining: impl FnMut() -> Duration,
    mut attempt: impl FnMut(&T) -> Result<(), E>,
) -> Result<bool, E> {
    for candidate in candidates.iter().take(AUTO_COMPACT_CHECKOUT_BATCH_SIZE) {
        if remaining() < crate::auto_cleanup::AUTO_COMPACTION_MIN_BUDGET {
            return Ok(false);
        }
        attempt(candidate)?;
    }
    Ok(true)
}

fn compact_checkout_worker_pass_budget(
    remaining: Duration,
    checkout_cleanup_due: bool,
) -> Option<Duration> {
    // The 90-second reserve caps scheduling; admission needs only enough
    // budget to preserve the full archive minimum after fresh index work.
    let cleanup_reserve = if checkout_cleanup_due {
        AUTO_CHECKOUT_CLEANUP_WORKER_RESERVE
    } else {
        Duration::ZERO
    };
    let compact_lane_remaining = remaining.saturating_sub(cleanup_reserve);
    (compact_lane_remaining >= AUTO_COMPACT_CHECKOUT_WORKER_ADMISSION_MINIMUM)
        .then_some(compact_lane_remaining.min(AUTO_COMPACT_CHECKOUT_WORKER_RESERVE))
}

fn compact_checkout_pending_is_due(
    backfill_pending: bool,
    backfill_retry_after_ms: Option<i64>,
    next_outbox_due_ms: Option<i64>,
    now: i64,
) -> bool {
    (backfill_pending && backfill_retry_after_ms.is_none_or(|retry| retry <= now))
        || next_outbox_due_ms.is_some_and(|due| due <= now)
}
/// Each whole-checkout pass has its own bounded tranche; the cursor resumes
/// after candidates inspected before this deadline.
const AUTO_CHECKOUT_CLEANUP_WORKER_PASS_BUDGET: Duration = Duration::from_secs(30);
/// A due session-retirement request keeps its own tranche in the detached
/// worker, even when the physical checkout-cleanup lane is cooling down.
const SESSION_RETIREMENT_WORKER_RESERVE: Duration = Duration::from_secs(30);
const SESSION_RETIREMENT_WORKER_PASS_BUDGET: Duration = Duration::from_secs(30);
// One pass can inspect up to 128 open sessions. Keep serialized report size
// bounded even when every inspected session has an unresolved reason.
const SESSION_RETIREMENT_WORKER_MAX_PASSES: usize = 16;
const SESSION_RETIREMENT_WORKER_RETRY_MS: i64 = 10 * 60_000;
/// A general foreground sweep reserves up to one cleanup-worker pass for
/// whole-checkout work, while leaving at least half of the pass for caches.
const AUTO_CHECKOUT_CLEANUP_FOREGROUND_RESERVE: Duration = AUTO_CHECKOUT_CLEANUP_WORKER_PASS_BUDGET;
/// Broker open checks only for rotation-debt control entries. A small bound
/// keeps this crash-gap probe cheap even when the shared cache root is wide.
const PREPARATION_ROTATION_DEBT_PROBE_MAX_ENTRIES: usize = 512;
const PREPARATION_ROTATION_DEBT_PROBE_BUDGET: Duration = Duration::from_millis(20);

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
    /// Finish: drain the artifact backlog without whole-checkout cleanup.
    FinishDrain,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactSweepLegacyUnscopedState {
    Clear,
    UnknownPending,
}

/// Result of an explicit `gc sweep`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ArtifactSweepReport {
    pub directories_reclaimed: usize,
    pub complete: bool,
    pub budget_ms: u64,
    pub disk_pressured: bool,
    /// Whether durable pre-namespace state remains outside this context's
    /// scoped sweep. The legacy values are inspected but never changed.
    pub legacy_unscoped_state: ArtifactSweepLegacyUnscopedState,
    /// Disposable checkouts removed or kept by this sweep (#588).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_cleanup: Option<crate::AutoCleanupReport>,
}

/// One complete circular lap, fetched in bounded pages. Querying the next page
/// is deferred until the caller has checked its deadline. The durable cursor
/// advances only when a candidate is actually taken by the caller.
struct ClosedSessionPages {
    start: i64,
    after: i64,
    wrapped: bool,
    only: Option<i64>,
    high_water: Option<i64>,
    done: bool,
}

impl ClosedSessionPages {
    fn new(cursor: i64, only: Option<i64>) -> Self {
        let start = cursor.max(0);
        Self {
            start,
            after: only.map_or(start, |id| id.saturating_sub(1)),
            wrapped: only.is_some(),
            only,
            high_water: None,
            done: false,
        }
    }

    fn next_page(
        &mut self,
        store: &crate::BrokerStore,
    ) -> Result<Vec<crate::Session>, crate::BrokerError> {
        if self.done {
            return Ok(Vec::new());
        }
        let high_water = match self.high_water {
            Some(id) => id,
            None => {
                let id = match self.only {
                    Some(id) => id,
                    None => store.cleaned_session_max_id()?,
                };
                self.high_water = Some(id);
                id
            }
        };
        loop {
            let through = self.only.unwrap_or(if self.wrapped {
                self.start.min(high_water)
            } else {
                high_water
            });
            let page = store.cleaned_session_page(self.after, Some(through), 32)?;
            if let Some(last) = page.last() {
                self.after = last.id;
                return Ok(page);
            }
            if self.wrapped || self.start == 0 {
                self.done = true;
                return Ok(Vec::new());
            }
            self.wrapped = true;
            self.after = 0;
        }
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterruptedTreeState {
    Resume,
    Revoked(&'static str),
    Blocked(&'static str),
    Unavailable(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum InterruptedIgnoredDirectoryKind {
    IgnoredDirectoryRoot,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct InterruptedIgnoredDirectory {
    kind: InterruptedIgnoredDirectoryKind,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct InterruptedIgnoredDirectoryQueue {
    version: u32,
    paths: Vec<InterruptedIgnoredDirectory>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RevokedInterruptedPath {
    path: String,
    reason: &'static str,
    canonical_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockedInterruptedPath {
    path: Option<String>,
    reason: &'static str,
}

fn bounded_artifact_diagnostic_path(path: &str) -> String {
    path.chars()
        .take(ARTIFACT_SWEEP_DIAGNOSTIC_PATH_MAX_CHARS)
        .collect()
}

struct ScopedArtifactSweepWriter<'a> {
    store: &'a mut crate::BrokerStore,
    namespace: &'a str,
}

impl ScopedArtifactSweepWriter<'_> {
    fn persist_interrupted_paths(&mut self, paths: &[String]) -> Result<bool, BrokerOpError> {
        persist_interrupted_paths(self.store, self.namespace, paths)
    }
}

fn revoke_interrupted_paths_under_root(
    sweep_writer: &mut ScopedArtifactSweepWriter<'_>,
    interrupted_paths: &mut Vec<String>,
    interrupted_paths_valid: &mut bool,
    root: &Path,
    reason: &'static str,
    revoked: &mut Vec<RevokedInterruptedPath>,
    blocked: &mut Vec<BlockedInterruptedPath>,
) -> Result<bool, BrokerOpError> {
    let revoked_paths = interrupted_paths
        .iter()
        .filter(|path| Path::new(path).starts_with(root))
        .cloned()
        .collect::<Vec<_>>();
    if revoked_paths.is_empty() {
        return Ok(true);
    }
    if !*interrupted_paths_valid {
        blocked.extend(
            revoked_paths
                .into_iter()
                .map(|path| BlockedInterruptedPath {
                    path: Some(path),
                    reason: "queue_unavailable_for_revocation",
                }),
        );
        return Ok(false);
    }
    let mut updated_paths = interrupted_paths.clone();
    updated_paths.retain(|path| !revoked_paths.contains(path));
    if sweep_writer.persist_interrupted_paths(&updated_paths)? {
        *interrupted_paths = updated_paths;
        revoked.extend(
            revoked_paths
                .into_iter()
                .map(|path| RevokedInterruptedPath {
                    path,
                    reason,
                    canonical_path: None,
                }),
        );
        Ok(true)
    } else {
        *interrupted_paths_valid = false;
        blocked.extend(
            revoked_paths
                .into_iter()
                .map(|path| BlockedInterruptedPath {
                    path: Some(path),
                    reason: "queue_update_unavailable",
                }),
        );
        Ok(false)
    }
}

/// Re-prove a remembered path before continuing removal. A known mismatch
/// revokes only this path's old removal authority; an I/O failure is different
/// because it says nothing about whether that authority is still valid.
fn interrupted_tree_state(root: &Path, path: &Path, extras: &[String]) -> InterruptedTreeState {
    let Ok(relative) = path.strip_prefix(root) else {
        return InterruptedTreeState::Revoked("outside_worktree");
    };
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty()
        || !components
            .iter()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return InterruptedTreeState::Revoked("unsafe_path_components");
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return InterruptedTreeState::Revoked("invalid_artifact_name");
    };
    if crate::artifact_catalog::unattended_witness(name, extras).is_none() {
        return InterruptedTreeState::Revoked("not_unattended_artifact");
    }
    let inspect_directory = |candidate: &Path| match std::fs::symlink_metadata(candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            InterruptedTreeState::Blocked("symlink_component")
        }
        Ok(metadata) if metadata.is_dir() => InterruptedTreeState::Resume,
        Ok(_) => InterruptedTreeState::Blocked("not_directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            InterruptedTreeState::Revoked("path_missing")
        }
        Err(_) => InterruptedTreeState::Unavailable("metadata_unavailable"),
    };
    let root_state = inspect_directory(root);
    if root_state != InterruptedTreeState::Resume {
        return root_state;
    }
    let mut walked = root.to_path_buf();
    for component in &components {
        walked.push(component);
        let state = inspect_directory(&walked);
        if state != InterruptedTreeState::Resume {
            return state;
        }
    }
    match (std::fs::canonicalize(root), std::fs::canonicalize(path)) {
        (Ok(canonical_root), Ok(canonical_path)) => {
            if canonical_path != canonical_root && canonical_path.starts_with(&canonical_root) {
                InterruptedTreeState::Resume
            } else {
                InterruptedTreeState::Blocked("outside_worktree")
            }
        }
        (Err(error), _) | (_, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            InterruptedTreeState::Revoked("path_missing")
        }
        _ => InterruptedTreeState::Unavailable("canonicalization_unavailable"),
    }
}

/// Read either the legacy single absolute path or the current bounded path
/// queue. Invalid and oversized state is kept fail-closed by the caller.
fn decode_interrupted_paths(value: Option<&str>) -> Result<Vec<String>, ()> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    if value.len() > ARTIFACT_SWEEP_INTERRUPTED_VALUE_MAX_BYTES {
        return Err(());
    }
    let decoded = if value.starts_with('[') {
        serde_json::from_str::<Vec<String>>(value).map_err(|_| ())?
    } else {
        vec![value.to_string()]
    };
    if decoded.len() > ARTIFACT_SWEEP_INTERRUPTED_PATHS_MAX
        || decoded.iter().any(|path| !Path::new(path).is_absolute())
    {
        return Err(());
    }
    let mut unique = Vec::with_capacity(decoded.len());
    let mut seen = BTreeSet::new();
    for path in decoded {
        if seen.insert(path.clone()) {
            unique.push(path);
        }
    }
    Ok(unique)
}

fn decode_interrupted_ignored_directories(value: Option<&str>) -> Result<Vec<String>, ()> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    if value.len() > ARTIFACT_SWEEP_INTERRUPTED_VALUE_MAX_BYTES {
        return Err(());
    }
    let queue: InterruptedIgnoredDirectoryQueue = serde_json::from_str(value).map_err(|_| ())?;
    if queue.version != IGNORED_DIRECTORY_QUEUE_SCHEMA_VERSION
        || queue.paths.len() > ARTIFACT_SWEEP_INTERRUPTED_PATHS_MAX
    {
        return Err(());
    }
    let mut paths = Vec::with_capacity(queue.paths.len());
    let mut seen = BTreeSet::new();
    for entry in queue.paths {
        let path = Path::new(&entry.path);
        if !path.is_absolute() || has_parent_dir_component(path) || !seen.insert(entry.path.clone())
        {
            return Err(());
        }
        paths.push(entry.path);
    }
    Ok(paths)
}

fn encode_interrupted_ignored_directories(paths: &[String]) -> Option<String> {
    if paths.len() > ARTIFACT_SWEEP_INTERRUPTED_PATHS_MAX {
        return None;
    }
    let mut entries = Vec::with_capacity(paths.len());
    let mut seen = BTreeSet::new();
    for path in paths {
        let candidate = Path::new(path);
        if !candidate.is_absolute()
            || has_parent_dir_component(candidate)
            || !seen.insert(path.clone())
        {
            return None;
        }
        entries.push(InterruptedIgnoredDirectory {
            kind: InterruptedIgnoredDirectoryKind::IgnoredDirectoryRoot,
            path: path.clone(),
        });
    }
    let value = if entries.is_empty() {
        String::new()
    } else {
        serde_json::to_string(&InterruptedIgnoredDirectoryQueue {
            version: IGNORED_DIRECTORY_QUEUE_SCHEMA_VERSION,
            paths: entries,
        })
        .ok()?
    };
    (value.len() <= ARTIFACT_SWEEP_INTERRUPTED_VALUE_MAX_BYTES).then_some(value)
}

fn persist_interrupted_ignored_directories(
    store: &mut crate::BrokerStore,
    namespace: &str,
    paths: &[String],
) -> Result<bool, BrokerOpError> {
    let Some(value) = encode_interrupted_ignored_directories(paths) else {
        return Ok(false);
    };
    store.meta_set(
        &scoped_artifact_sweep_key(namespace, ARTIFACT_SWEEP_IGNORED_DIR_INTERRUPTED_KEY),
        &value,
    )?;
    Ok(true)
}

fn ignored_directory_sweep_policy_allows_checkout(
    policy: &crate::AutoCleanupPolicy,
    worktree: &Path,
) -> bool {
    policy.ignored_directory_sweep && !policy.strict && !policy.is_pinned(worktree)
}

fn ignored_listing_has_directory_root(
    entries: &[crate::git::GitIgnoredPathEntry],
    relative: &str,
) -> bool {
    entries.iter().any(|entry| {
        entry.relative == relative
            && entry.directory_marker
            && entry.kind == crate::git::GitIgnoredPathKind::DirectoryRoot
    })
}

fn has_parent_dir_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

/// Keep a one-path marker compatible with older brokers and encode multiple
/// interrupted trees without ambiguous path delimiters.
fn encode_interrupted_paths(paths: &[String]) -> Option<String> {
    if paths.len() > ARTIFACT_SWEEP_INTERRUPTED_PATHS_MAX
        || paths.iter().any(|path| !Path::new(path).is_absolute())
    {
        return None;
    }
    let value = match paths {
        [] => String::new(),
        [path] => path.clone(),
        paths => serde_json::Value::Array(
            paths
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        )
        .to_string(),
    };
    (value.len() <= ARTIFACT_SWEEP_INTERRUPTED_VALUE_MAX_BYTES).then_some(value)
}

fn persist_interrupted_paths(
    store: &mut crate::BrokerStore,
    namespace: &str,
    paths: &[String],
) -> Result<bool, BrokerOpError> {
    let Some(value) = encode_interrupted_paths(paths) else {
        return Ok(false);
    };
    store.meta_set(
        &scoped_artifact_sweep_key(namespace, ARTIFACT_SWEEP_INTERRUPTED_KEY),
        &value,
    )?;
    Ok(true)
}

/// Drop paths that are conclusively absent or no longer directories. Keep
/// symlinks for the per-worktree authority check, and preserve metadata errors
/// as blockers rather than mistaking them for completed removals.
fn prune_absent_interrupted_paths(paths: &mut Vec<String>) -> Vec<String> {
    let mut unavailable = Vec::new();
    paths.retain(|path| match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => true,
        Ok(metadata) if metadata.is_dir() => true,
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            unavailable.push(path.clone());
            true
        }
    });
    unavailable
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
        | ArtifactSweepScope::FinishDrain
        | ArtifactSweepScope::AutomaticContinuation
        | ArtifactSweepScope::ClosedSession(_) => {
            policy_budget_ms.saturating_mul(urgency.budget_scale())
        }
    }
}

fn scope_runs_checkout_cleanup(scope: ArtifactSweepScope) -> bool {
    !matches!(
        scope,
        ArtifactSweepScope::FinishDrain
            | ArtifactSweepScope::AutomaticContinuation
            | ArtifactSweepScope::ClosedSession(_)
    )
}

fn foreground_checkout_cleanup_reserve(
    budget_ms: u64,
    scope: ArtifactSweepScope,
    auto_remove_enabled: bool,
) -> Duration {
    if !scope_runs_checkout_cleanup(scope) || !auto_remove_enabled {
        return Duration::ZERO;
    }
    let reserve_ms =
        (budget_ms / 2).min(AUTO_CHECKOUT_CLEANUP_FOREGROUND_RESERVE.as_millis() as u64);
    Duration::from_millis(reserve_ms)
}

#[derive(Debug, Clone, Copy)]
struct ArtifactSweepDeadlines {
    discovery: Instant,
    removal: Instant,
    checkout_cleanup: Instant,
}

fn artifact_sweep_deadlines(
    started_at: Instant,
    budget_ms: u64,
    checkout_cleanup_reserve: Duration,
    discovery_override: Option<Instant>,
    removal_override: Option<Instant>,
    checkout_cleanup_override: Option<Instant>,
) -> ArtifactSweepDeadlines {
    let budget_deadline = started_at + Duration::from_millis(budget_ms);
    let cache_deadline = budget_deadline
        .checked_sub(checkout_cleanup_reserve)
        .unwrap_or(started_at);
    let discovery = discovery_override.unwrap_or(cache_deadline);
    let removal = removal_override.unwrap_or(cache_deadline);
    // Keep the existing deadline override seam meaningful for tests. In
    // production, no override is supplied and checkout cleanup gets the
    // remainder of the original overall sweep budget.
    let checkout_cleanup =
        checkout_cleanup_override.unwrap_or_else(|| match (discovery_override, removal_override) {
            (None, None) => budget_deadline,
            (discovery, removal) => discovery
                .unwrap_or(budget_deadline)
                .min(removal.unwrap_or(budget_deadline)),
        });
    ArtifactSweepDeadlines {
        discovery,
        removal,
        checkout_cleanup,
    }
}

fn is_real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

fn days_between(now: i64, earlier: i64) -> u32 {
    u32::try_from(now.saturating_sub(earlier) / MILLIS_PER_DAY).unwrap_or(u32::MAX)
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

struct BoundedArtifactDiscovery {
    classified: Vec<PathBuf>,
    complete: bool,
    entries_examined: usize,
}

struct IgnoredDirectoryDiscovery {
    entries: Vec<crate::git::GitIgnoredPathEntry>,
    generic_roots: BTreeSet<PathBuf>,
    opaque_roots: BTreeSet<PathBuf>,
    complete: bool,
    entries_examined: usize,
}

fn discover_ignored_directory_roots_bounded(
    root: &Path,
    checkout: &GitRepo,
    extras: &[String],
    deadline: Instant,
    entry_budget: usize,
) -> Result<IgnoredDirectoryDiscovery, ()> {
    let _git_deadline = crate::git::limit_git_until(deadline);
    let entries = checkout
        .ignored_paths_readonly_until(deadline)
        .map_err(|_| ())?;
    let mut discovery = IgnoredDirectoryDiscovery {
        entries,
        generic_roots: BTreeSet::new(),
        opaque_roots: BTreeSet::new(),
        complete: true,
        entries_examined: 0,
    };
    if discovery.entries.len() > entry_budget {
        discovery.complete = false;
    }
    let admitted = discovery.entries.len().min(entry_budget);
    let mut candidates = Vec::new();
    for entry in discovery.entries.iter().take(admitted) {
        if Instant::now() >= deadline {
            discovery.complete = false;
            break;
        }
        discovery.entries_examined = discovery.entries_examined.saturating_add(1);
        match entry.kind {
            crate::git::GitIgnoredPathKind::DirectoryRoot
            | crate::git::GitIgnoredPathKind::Protected => {
                let path = root.join(&entry.relative);
                discovery.opaque_roots.insert(path.clone());
                if entry.kind == crate::git::GitIgnoredPathKind::DirectoryRoot {
                    let depth = Path::new(&entry.relative).components().count();
                    if depth > ARTIFACT_SCAN_DEPTH {
                        discovery.complete = false;
                    } else if artifact_witness_for_with_extras(&path, extras).is_none() {
                        candidates.push(path);
                    }
                }
            }
            crate::git::GitIgnoredPathKind::Other => {}
        }
    }
    if Instant::now() >= deadline {
        discovery.complete = false;
    }
    // Prefer an outer opaque ignored folder so descendants are never scanned
    // or queued as a second removal target.
    candidates.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    for candidate in candidates {
        if !discovery
            .generic_roots
            .iter()
            .any(|parent| candidate.starts_with(parent))
        {
            discovery.generic_roots.insert(candidate);
        }
    }
    Ok(discovery)
}

impl BoundedArtifactDiscovery {
    fn new() -> Self {
        Self {
            classified: Vec::new(),
            complete: true,
            entries_examined: 0,
        }
    }
}

fn collect_artifact_dirs_bounded(
    root: &Path,
    extras: &[String],
    deadline: Instant,
    entry_budget: usize,
) -> BoundedArtifactDiscovery {
    collect_artifact_dirs_bounded_with_opaque_ignored_roots(
        root,
        extras,
        &BTreeSet::new(),
        deadline,
        entry_budget,
    )
}

fn collect_artifact_dirs_bounded_with_opaque_ignored_roots(
    root: &Path,
    extras: &[String],
    opaque_ignored_roots: &BTreeSet<PathBuf>,
    deadline: Instant,
    entry_budget: usize,
) -> BoundedArtifactDiscovery {
    fn visit(
        current: &Path,
        depth: usize,
        extras: &[String],
        opaque_ignored_roots: &BTreeSet<PathBuf>,
        deadline: Instant,
        entry_budget: usize,
        discovery: &mut BoundedArtifactDiscovery,
    ) {
        if !discovery.complete {
            return;
        }
        if depth > ARTIFACT_SCAN_DEPTH {
            // A depth cap is a discovery limit just like the entry budget: a
            // hidden cache may exist below this point, so the caller must not
            // advance the completed generation.
            discovery.complete = false;
            return;
        }
        if Instant::now() >= deadline || discovery.entries_examined >= entry_budget {
            discovery.complete = false;
            return;
        }
        let entries = match std::fs::read_dir(current) {
            Ok(entries) => entries,
            Err(_) => {
                discovery.complete = false;
                return;
            }
        };
        for entry in entries {
            if Instant::now() >= deadline || discovery.entries_examined >= entry_budget {
                discovery.complete = false;
                return;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    discovery.complete = false;
                    return;
                }
            };
            discovery.entries_examined = discovery.entries_examined.saturating_add(1);
            let path = entry.path();
            if !is_real_directory(&path) {
                continue;
            }
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if artifact_witness_for_with_extras(&path, extras).is_some() {
                discovery.classified.push(path);
                continue;
            }
            if opaque_ignored_roots.contains(&path) {
                // A complete Git listing already classified this ignored root.
                // Do not descend into its opaque contents looking for nested
                // cache names; the opt-in lane handles the root as one unit.
                continue;
            }
            visit(
                &path,
                depth + 1,
                extras,
                opaque_ignored_roots,
                deadline,
                entry_budget,
                discovery,
            );
            if !discovery.complete {
                return;
            }
        }
    }

    let mut discovery = BoundedArtifactDiscovery::new();
    visit(
        root,
        0,
        extras,
        opaque_ignored_roots,
        deadline,
        entry_budget,
        &mut discovery,
    );
    discovery
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

/// The measuring CLI scan also reports ignored directories outside the
/// artifact catalog. Each unknown directory may require a Git subprocess, so
/// the discovery walk shares the plan deadline and an entry cap with sizing.
fn collect_artifact_scan_until(
    root: &Path,
    extras: &[String],
    checkout: &GitRepo,
    deadline: Instant,
    entry_budget: usize,
) -> (ArtifactScan, bool) {
    let _git_deadline = crate::git::limit_git_until(deadline);

    #[allow(clippy::too_many_arguments)]
    fn visit(
        root: &Path,
        current: &Path,
        depth: usize,
        extras: &[String],
        checkout: &GitRepo,
        deadline: Instant,
        entry_budget: usize,
        entries_examined: &mut usize,
        scan: &mut ArtifactScan,
    ) -> bool {
        if Instant::now() >= deadline || *entries_examined >= entry_budget {
            return false;
        }
        if depth > ARTIFACT_SCAN_DEPTH {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(current) else {
            return false;
        };
        for entry in entries {
            if Instant::now() >= deadline || *entries_examined >= entry_budget {
                return false;
            }
            let Ok(entry) = entry else {
                return false;
            };
            *entries_examined = (*entries_examined).saturating_add(1);
            let path = entry.path();
            if !is_real_directory(&path) || path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if artifact_witness_for_with_extras(&path, extras).is_some() {
                scan.classified.push(path);
                continue;
            }
            let name = path.file_name().and_then(|name| name.to_str());
            let is_unknown_name = name.is_some_and(|name| !is_known_artifact_name(name, extras));
            if is_unknown_name && let Some(relative) = repo_relative(root, &path) {
                if Instant::now() >= deadline {
                    return false;
                }
                let ignored = match checkout.path_is_ignored_checked(&relative) {
                    Ok(ignored) => ignored,
                    // A command failure is not evidence that the directory
                    // is safe to descend into or report as unignored.
                    Err(_) => return false,
                };
                if ignored {
                    scan.declined.push(path);
                    continue;
                }
            }
            if !visit(
                root,
                &path,
                depth + 1,
                extras,
                checkout,
                deadline,
                entry_budget,
                entries_examined,
                scan,
            ) {
                return false;
            }
        }
        true
    }

    let mut scan = ArtifactScan::default();
    let mut entries_examined = 0;
    let complete = visit(
        root,
        root,
        0,
        extras,
        checkout,
        deadline,
        entry_budget,
        &mut entries_examined,
        &mut scan,
    );
    (scan, complete)
}

fn declined_artifacts(
    session_id: i64,
    worktree_path: &str,
    root: &Path,
    paths: Vec<PathBuf>,
    extras: &[String],
    deadline: Option<Instant>,
) -> (Vec<GcDeclinedArtifact>, bool) {
    let mut declined = Vec::new();
    for path in paths {
        if deadline_expired(deadline) {
            return (declined, false);
        }
        let Some(relative_dir) = repo_relative(root, &path) else {
            continue;
        };
        let usage = match deadline {
            Some(deadline) => crate::disk_headroom::directory_usage_bounded(&path, deadline),
            None => crate::disk_headroom::directory_usage_without_following_links(&path).ok(),
        };
        let Some(usage) = usage else {
            if deadline.is_some() {
                return (declined, false);
            }
            continue;
        };
        if usage.bytes > UNCLASSIFIED_ARTIFACT_REPORT_THRESHOLD_BYTES {
            declined.push(GcDeclinedArtifact {
                session_id,
                worktree_path: worktree_path.to_owned(),
                relative_dir,
                estimated_bytes: usage.bytes,
                estimated_inodes: Some(usage.inodes),
                reason: declined_reason(&path, extras).into(),
            });
        }
    }
    (declined, true)
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

fn recovery_archive_minimum_age_elapsed(
    archive: &crate::cleanup_resolve::OwnedRecoveryArchive,
    now: i64,
) -> bool {
    let minimum_age_ms = archive
        .minimum_archive_retention_ms
        .unwrap_or(crate::retention::MIN_RECOVERY_ARCHIVE_RETENTION_MS);
    now.checked_sub(archive.created_at_ms)
        .and_then(|age| u64::try_from(age).ok())
        .is_some_and(|age| age >= minimum_age_ms)
}

#[cfg(test)]
mod recovery_archive_minimum_age_tests {
    use super::recovery_archive_minimum_age_elapsed;
    use crate::cleanup_resolve::OwnedRecoveryArchive;
    use crate::retention::MIN_RECOVERY_ARCHIVE_RETENTION_MS;

    #[test]
    fn immutable_and_legacy_age_use_exact_boundary_and_fail_closed_on_clock_overflow() {
        let mut archive = OwnedRecoveryArchive {
            path: "/fixture/archive".into(),
            session_id: 1,
            head: "a".repeat(40),
            recovery_roots: vec![crate::cleanup_resolve::RecoveryRootProof {
                ref_name: "HEAD".into(),
                object_id: "a".repeat(40),
                available_from_delivery: false,
            }],
            created_at_ms: 100,
            minimum_archive_retention_ms: Some(MIN_RECOVERY_ARCHIVE_RETENTION_MS),
            prerequisite_refs: Vec::new(),
            retirement_digest: None,
            prerequisite_targets: Vec::new(),
            holds_uncommitted: false,
        };
        let minimum_age_ms = i64::try_from(MIN_RECOVERY_ARCHIVE_RETENTION_MS).unwrap();
        assert!(!recovery_archive_minimum_age_elapsed(&archive, 99));
        assert!(!recovery_archive_minimum_age_elapsed(
            &archive,
            100 + minimum_age_ms - 1
        ));
        assert!(recovery_archive_minimum_age_elapsed(
            &archive,
            100 + minimum_age_ms
        ));
        archive.created_at_ms = i64::MIN;
        assert!(!recovery_archive_minimum_age_elapsed(&archive, i64::MAX));
        archive.created_at_ms = 0;
        archive.minimum_archive_retention_ms = Some(u64::MAX);
        assert!(!recovery_archive_minimum_age_elapsed(&archive, i64::MAX));
        archive.created_at_ms = 100;
        archive.minimum_archive_retention_ms = None;
        assert!(!recovery_archive_minimum_age_elapsed(&archive, 99));
        assert!(!recovery_archive_minimum_age_elapsed(
            &archive,
            100 + minimum_age_ms - 1
        ));
        assert!(recovery_archive_minimum_age_elapsed(
            &archive,
            100 + minimum_age_ms
        ));
        archive.created_at_ms = i64::MIN;
        assert!(!recovery_archive_minimum_age_elapsed(&archive, i64::MAX));
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
    // The archive carries this promise independently of current policy or
    // landing. The same helper guards both planning and journal replay.
    if !recovery_archive_minimum_age_elapsed(archive, now) || policy.recovery_archive_days == 0 {
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
    now.saturating_sub(i64::from(days).saturating_mul(MILLIS_PER_DAY))
}

/// Floor an exact timestamp to midnight UTC, so every age-based decision in a
/// new plan remains stable for the rest of that UTC day. This is deliberately
/// separate from `evaluated_at`, which remains the actual observation time.
fn utc_day_floor_ms(timestamp_ms: i64) -> i64 {
    timestamp_ms
        .div_euclid(MILLIS_PER_DAY)
        .saturating_mul(MILLIS_PER_DAY)
}

fn sum_inode_estimates(values: impl IntoIterator<Item = Option<u64>>) -> Option<u64> {
    values.into_iter().try_fold(0_u64, |total, estimate| {
        estimate.map(|inodes| total.saturating_add(inodes))
    })
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn open_regular_file_nonblocking(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path did not open as a regular file",
        ));
    }
    Ok(file)
}

fn same_regular_file_version(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    if !left.is_file()
        || !right.is_file()
        || left.len() != right.len()
        || left
            .modified()
            .ok()
            .zip(right.modified().ok())
            .is_none_or(|(left, right)| left != right)
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.ctime() == right.ctime()
            && left.ctime_nsec() == right.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        true
    }
}

const GC_FILE_READ_CHUNK_BYTES: usize = 64 * 1024;
const GC_METRICS_HEALTH_READ_LIMIT_BYTES: usize = 64 * 1024 * 1024;

fn sha256_reader_until_with_clock<R, C>(
    reader: &mut R,
    deadline: Option<Instant>,
    mut now: C,
) -> std::io::Result<Option<(String, u64)>>
where
    R: Read,
    C: FnMut() -> Instant,
{
    let mut digest = Sha256::new();
    let mut bytes_read = 0_u64;
    let mut buffer = [0_u8; GC_FILE_READ_CHUNK_BYTES];
    loop {
        if deadline.is_some_and(|deadline| now() >= deadline) {
            return Ok(None);
        }
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            if deadline.is_some_and(|deadline| now() >= deadline) {
                return Ok(None);
            }
            return Ok(Some((format!("{:x}", digest.finalize()), bytes_read)));
        }
        digest.update(&buffer[..count]);
        bytes_read = bytes_read.saturating_add(count as u64);
    }
}

fn sha256_file_until(
    path: &Path,
    deadline: Option<Instant>,
) -> std::io::Result<Option<(String, u64)>> {
    if deadline_expired(deadline) {
        return Ok(None);
    }
    let mut file = open_regular_file_nonblocking(path)?;
    let opened = file.metadata()?;
    let before_path = std::fs::symlink_metadata(path)?;
    if !same_regular_file_version(&opened, &before_path) || deadline_expired(deadline) {
        return Ok(None);
    }
    let Some((digest, bytes)) = sha256_reader_until_with_clock(&mut file, deadline, Instant::now)?
    else {
        return Ok(None);
    };
    let after_open = file.metadata()?;
    let after_path = std::fs::symlink_metadata(path)?;
    if deadline_expired(deadline)
        || !same_regular_file_version(&opened, &after_open)
        || !same_regular_file_version(&opened, &after_path)
        || bytes != opened.len()
    {
        return Ok(None);
    }
    Ok(Some((digest, bytes)))
}

fn read_all_until_with_clock<R, C>(
    reader: &mut R,
    deadline: Option<Instant>,
    mut now: C,
) -> std::io::Result<Option<Vec<u8>>>
where
    R: Read,
    C: FnMut() -> Instant,
{
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; GC_FILE_READ_CHUNK_BYTES];
    loop {
        if deadline.is_some_and(|deadline| now() >= deadline) {
            return Ok(None);
        }
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            if deadline.is_some_and(|deadline| now() >= deadline) {
                return Ok(None);
            }
            return Ok(Some(bytes));
        }
        if deadline.is_some()
            && bytes.len().saturating_add(count) > GC_METRICS_HEALTH_READ_LIMIT_BYTES
        {
            return Ok(None);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

fn retained_metric_bytes_until(
    bytes: &[u8],
    cutoff: i64,
    deadline: Option<Instant>,
) -> Option<(Vec<u8>, bool, usize)> {
    let mut after = Vec::with_capacity(bytes.len());
    let mut removed = false;
    let mut malformed_lines = 0_usize;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if deadline_expired(deadline) {
            return None;
        }
        let timestamp = serde_json::from_slice::<serde_json::Value>(line)
            .ok()
            .and_then(|value| value.get("ts").and_then(serde_json::Value::as_i64));
        match timestamp {
            Some(timestamp) if timestamp < cutoff => removed = true,
            Some(_) => after.extend_from_slice(line),
            None => {
                malformed_lines = malformed_lines.saturating_add(1);
                after.extend_from_slice(line);
            }
        }
    }
    Some((after, removed, malformed_lines))
}

fn retained_metric_bytes(bytes: &[u8], cutoff: i64) -> Vec<u8> {
    retained_metric_bytes_until(bytes, cutoff, None)
        .expect("an unbounded metrics rewrite completes")
        .0
}

fn repo_relative(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

enum MetricsRewrite {
    Complete(Option<GcFileCandidate>),
    Deferred,
}

fn metrics_rewrite(
    main_root: &Path,
    cutoff: i64,
    blockers: &mut Vec<GcBlocker>,
    deadline: Option<Instant>,
) -> Result<MetricsRewrite, BrokerOpError> {
    let relative = ".aethyme/logs/command-metrics.jsonl";
    let path = main_root.join(relative);
    if deadline_expired(deadline) {
        return Ok(MetricsRewrite::Deferred);
    }
    if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        blockers.push(GcBlocker {
            kind: "command_metrics".into(),
            id: None,
            reason: format!("{relative} is a symlink and is never rewritten by GC"),
        });
        return Ok(MetricsRewrite::Complete(None));
    }
    let mut file = match open_regular_file_nonblocking(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MetricsRewrite::Complete(None));
        }
        Err(error) => {
            blockers.push(GcBlocker {
                kind: "command_metrics".into(),
                id: None,
                reason: format!("cannot inspect {relative}: {error}"),
            });
            return Ok(MetricsRewrite::Deferred);
        }
    };
    let opened = file.metadata().map_err(|source| crate::BrokerError::Io {
        path: path.clone(),
        source,
    })?;
    let before_path =
        std::fs::symlink_metadata(&path).map_err(|source| crate::BrokerError::Io {
            path: path.clone(),
            source,
        })?;
    if !same_regular_file_version(&opened, &before_path) || deadline_expired(deadline) {
        return Ok(MetricsRewrite::Deferred);
    }
    let bytes = match read_all_until_with_clock(&mut file, deadline, Instant::now) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(MetricsRewrite::Deferred),
        Err(error) => {
            blockers.push(GcBlocker {
                kind: "command_metrics".into(),
                id: None,
                reason: format!("cannot inspect {relative}: {error}"),
            });
            return Ok(MetricsRewrite::Deferred);
        }
    };
    let after_open = file.metadata().map_err(|source| crate::BrokerError::Io {
        path: path.clone(),
        source,
    })?;
    let after_path = std::fs::symlink_metadata(&path).map_err(|source| crate::BrokerError::Io {
        path: path.clone(),
        source,
    })?;
    if deadline_expired(deadline)
        || !same_regular_file_version(&opened, &after_open)
        || !same_regular_file_version(&opened, &after_path)
        || bytes.len() as u64 != opened.len()
    {
        return Ok(MetricsRewrite::Deferred);
    }
    let Some((after, removed, malformed_lines)) =
        retained_metric_bytes_until(&bytes, cutoff, deadline)
    else {
        return Ok(MetricsRewrite::Deferred);
    };
    if deadline.is_some() && malformed_lines > 0 {
        // Health only needs a conservative blocker. Keep this summary bounded
        // instead of allocating one blocker per malformed input line.
        blockers.push(GcBlocker {
            kind: "command_metric_line".into(),
            id: None,
            reason: format!(
                "retained {malformed_lines} malformed or timestamp-free command metric line(s)"
            ),
        });
    } else {
        // Preserve the full-GC report's historical one-blocker-per-line shape.
        for _ in 0..malformed_lines {
            blockers.push(GcBlocker {
                kind: "command_metric_line".into(),
                id: None,
                reason: "retained one malformed or timestamp-free command metric line".into(),
            });
        }
    }
    if !removed {
        return Ok(MetricsRewrite::Complete(None));
    }
    Ok(MetricsRewrite::Complete(Some(GcFileCandidate {
        path: relative.into(),
        action: GcFileAction::Rewrite,
        before_sha256: sha256(&bytes),
        after_sha256: Some(sha256(&after)),
        bytes_before: bytes.len() as u64,
        bytes_after: after.len() as u64,
        source_row_ids: Vec::new(),
    })))
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct GcJournal {
    schema_version: u32,
    digest: String,
    evaluated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    age_anchor_ms: Option<i64>,
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

/// Resolve the age reference used to resume a reviewed journal. Schema 3 did
/// not carry an anchor, so it must keep using its exact evaluation timestamp.
/// Schema 4 binds the UTC-day floor into the plan digest and must carry that
/// exact floor in the journal.
fn gc_journal_age_reference_ms(journal: &GcJournal) -> Result<i64, BrokerOpError> {
    match journal.schema_version {
        GC_LEGACY_PLAN_SCHEMA_VERSION => Ok(journal.evaluated_at),
        GC_PLAN_SCHEMA_VERSION => {
            let Some(age_anchor_ms) = journal.age_anchor_ms else {
                return Err(BrokerOpError::InvalidGcJournal {
                    reason: "schema 4 journal is missing its digest-bound UTC-day age anchor"
                        .into(),
                });
            };
            if age_anchor_ms != utc_day_floor_ms(journal.evaluated_at) {
                return Err(BrokerOpError::InvalidGcJournal {
                    reason: "schema 4 journal age anchor is not the UTC-day floor of evaluated_at"
                        .into(),
                });
            }
            Ok(age_anchor_ms)
        }
        version => Err(BrokerOpError::InvalidGcJournal {
            reason: format!("schema {version} has no resumable GC age reference"),
        }),
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct GcJournalAbandonReport {
    pub(crate) digest: String,
    pub(crate) archive_path: String,
    pub(crate) bytes_archived: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct GcJournalForgetReport {
    pub(crate) digest: String,
    pub(crate) bytes_forgotten: u64,
}

impl From<GcPlan> for GcJournal {
    fn from(plan: GcPlan) -> Self {
        Self {
            schema_version: plan.schema_version,
            digest: plan.digest,
            evaluated_at: plan.evaluated_at,
            age_anchor_ms: plan.age_anchor_ms,
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
        Ok(metadata) if metadata.file_type().is_file() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(worker_path_error(
                        &lock_path,
                        "worker lock file must not have hard links",
                    ));
                }
            }
        }
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

/// Keep cache-only continuation outside the launching broker's owned process
/// group. Agent and inline supervisors tear down their group after the launcher
/// exits; the worker's durable request/cursor make this independent lifetime
/// recoverable without leaving it attached to that cleanup.
#[cfg(unix)]
fn configure_artifact_worker_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_artifact_worker_process_group(_: &mut Command) {
    // This continuation remains best-effort on platforms without Unix groups.
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
                crate::warn_unrecorded(
                    "stop timed-out artifact worker version probe",
                    child.kill(),
                );
                crate::warn_unrecorded(
                    "reap timed-out artifact worker version probe",
                    child.wait(),
                );
                return None;
            }
            Err(error) => {
                crate::warn_unrecorded::<(), _>(
                    "inspect artifact worker version probe",
                    Err(error),
                );
                crate::warn_unrecorded("stop failed artifact worker version probe", child.kill());
                crate::warn_unrecorded("reap failed artifact worker version probe", child.wait());
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
    let router_version = worker_binary_version(&router);
    let engine_version = worker_binary_version(&engine);
    let router_identity = router_version
        .as_deref()
        .and_then(crate::install_health::build_identity);
    let engine_identity = engine_version
        .as_deref()
        .and_then(crate::install_health::build_identity);
    matches!(
        crate::install_health::pair_state(router_identity, engine_identity),
        crate::install_health::PairState::Aligned(_)
    )
}

fn worker_retry_pending(
    store: &crate::BrokerStore,
    namespace: &str,
    now: i64,
) -> Result<bool, BrokerOpError> {
    Ok(store
        .meta_get(&scoped_artifact_sweep_key(
            namespace,
            ARTIFACT_SWEEP_RETRY_AFTER_KEY,
        ))?
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
        if named.dev() != opened.dev()
            || named.ino() != opened.ino()
            || named.nlink() != 1
            || opened.nlink() != 1
        {
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
#[cfg(test)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GcStateDirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GcStateFileIdentity {
    device: u64,
    inode: u64,
}

struct GcStateFileSnapshot {
    bytes: Vec<u8>,
    identity: GcStateFileIdentity,
    file: std::fs::File,
}

#[cfg(unix)]
fn gc_state_file_link_count(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink()
}

#[cfg(not(unix))]
fn gc_state_file_link_count(_metadata: &std::fs::Metadata) -> u64 {
    1
}

fn gc_archive_refusal(reason: impl Into<String>) -> BrokerOpError {
    BrokerOpError::GcJournalAbandonRefused {
        reason: reason.into(),
    }
}

fn gc_io(path: &Path, source: std::io::Error) -> BrokerOpError {
    crate::BrokerError::Io {
        path: path.to_path_buf(),
        source,
    }
    .into()
}

#[cfg(unix)]
fn gc_state_directory_identity(path: &Path) -> Result<GcStateDirectoryIdentity, BrokerOpError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path).map_err(|source| gc_io(path, source))?;
    // SAFETY: geteuid has no pointer preconditions.
    let uid = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.mode() & 0o022 != 0
    {
        return Err(gc_archive_refusal(format!(
            "state directory is not an owned, non-writable real directory: {}",
            path.display()
        )));
    }
    Ok(GcStateDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn gc_state_directory_identity(_path: &Path) -> Result<GcStateDirectoryIdentity, BrokerOpError> {
    Err(gc_archive_refusal(
        "safe no-follow journal archival is supported only on Unix",
    ))
}

#[cfg(unix)]
fn gc_state_file_identity_with_link_limit(
    metadata: &std::fs::Metadata,
    path: &Path,
    max_links: u64,
) -> Result<GcStateFileIdentity, BrokerOpError> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: geteuid has no pointer preconditions.
    let uid = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() == 0
        || metadata.nlink() > max_links
        || metadata.mode() & 0o077 != 0
    {
        return Err(gc_archive_refusal(format!(
            "journal path is not a private, owned regular file: {}",
            path.display()
        )));
    }
    Ok(GcStateFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn gc_state_file_identity_with_link_limit(
    _metadata: &std::fs::Metadata,
    _path: &Path,
    _max_links: u64,
) -> Result<GcStateFileIdentity, BrokerOpError> {
    Err(gc_archive_refusal(
        "safe no-follow journal archival is supported only on Unix",
    ))
}

#[cfg(unix)]
fn read_gc_state_file(
    path: &Path,
    parent: &Path,
    parent_identity: GcStateDirectoryIdentity,
) -> Result<Option<GcStateFileSnapshot>, BrokerOpError> {
    read_gc_state_file_with_link_limit(path, parent, parent_identity, 1)
}

#[cfg(not(unix))]
fn read_gc_state_file(
    _path: &Path,
    _parent: &Path,
    _parent_identity: GcStateDirectoryIdentity,
) -> Result<Option<GcStateFileSnapshot>, BrokerOpError> {
    Err(gc_archive_refusal(
        "safe no-follow journal archival is supported only on Unix",
    ))
}

#[cfg(unix)]
fn read_gc_state_file_with_link_limit(
    path: &Path,
    parent: &Path,
    parent_identity: GcStateDirectoryIdentity,
    max_links: u64,
) -> Result<Option<GcStateFileSnapshot>, BrokerOpError> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    if gc_state_directory_identity(parent)? != parent_identity {
        return Err(gc_archive_refusal(
            "state directory identity changed during journal archival",
        ));
    }
    let before = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(gc_io(path, source)),
    };
    let before_identity = gc_state_file_identity_with_link_limit(&before, path, max_links)?;
    if before.len() > GC_JOURNAL_ARCHIVE_MAX_BYTES {
        return Err(gc_archive_refusal(format!(
            "journal file exceeds the {} byte archive limit: {}",
            GC_JOURNAL_ARCHIVE_MAX_BYTES,
            path.display()
        )));
    }
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let mut file = options.open(path).map_err(|source| gc_io(path, source))?;
    let opened = file.metadata().map_err(|source| gc_io(path, source))?;
    let opened_identity = gc_state_file_identity_with_link_limit(&opened, path, max_links)?;
    if opened_identity != before_identity {
        return Err(gc_archive_refusal(format!(
            "journal file identity changed while opening: {}",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    {
        let mut bounded =
            std::io::Read::by_ref(&mut file).take(GC_JOURNAL_ARCHIVE_MAX_BYTES.saturating_add(1));
        bounded
            .read_to_end(&mut bytes)
            .map_err(|source| gc_io(path, source))?;
    }
    if bytes.len() as u64 > GC_JOURNAL_ARCHIVE_MAX_BYTES {
        return Err(gc_archive_refusal(format!(
            "journal file grew beyond the {} byte archive limit while reading: {}",
            GC_JOURNAL_ARCHIVE_MAX_BYTES,
            path.display()
        )));
    }
    let after = std::fs::symlink_metadata(path).map_err(|source| gc_io(path, source))?;
    let after_identity = gc_state_file_identity_with_link_limit(&after, path, max_links)?;
    let opened_after_read = file.metadata().map_err(|source| gc_io(path, source))?;
    if after_identity != opened_identity
        || after.len() != bytes.len() as u64
        || opened_after_read.len() != bytes.len() as u64
    {
        return Err(gc_archive_refusal(format!(
            "journal file changed while reading: {}",
            path.display()
        )));
    }
    if gc_state_directory_identity(parent)? != parent_identity {
        return Err(gc_archive_refusal(
            "state directory identity changed while reading the journal",
        ));
    }
    Ok(Some(GcStateFileSnapshot {
        bytes,
        identity: opened_identity,
        file,
    }))
}

#[cfg(not(unix))]
fn read_gc_state_file_with_link_limit(
    _path: &Path,
    _parent: &Path,
    _parent_identity: GcStateDirectoryIdentity,
    _max_links: u64,
) -> Result<Option<GcStateFileSnapshot>, BrokerOpError> {
    Err(gc_archive_refusal(
        "safe no-follow journal archival is supported only on Unix",
    ))
}

#[cfg(unix)]
fn sync_gc_state_directory(
    parent: &Path,
    expected_identity: GcStateDirectoryIdentity,
) -> Result<(), BrokerOpError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let directory = options
        .open(parent)
        .map_err(|source| gc_io(parent, source))?;
    let metadata = directory
        .metadata()
        .map_err(|source| gc_io(parent, source))?;
    // SAFETY: geteuid has no pointer preconditions.
    let uid = unsafe { libc::geteuid() };
    let identity = GcStateDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    if !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.mode() & 0o022 != 0
        || identity != expected_identity
    {
        return Err(gc_archive_refusal(
            "state directory identity or ownership changed before it could be synced",
        ));
    }
    directory.sync_all().map_err(|source| gc_io(parent, source))
}

#[cfg(not(unix))]
fn sync_gc_state_directory(
    _parent: &Path,
    _expected_identity: GcStateDirectoryIdentity,
) -> Result<(), BrokerOpError> {
    Err(gc_archive_refusal(
        "safe no-follow journal archival is supported only on Unix",
    ))
}

#[cfg(unix)]
fn remove_interrupted_gc_archive_link(
    parent: &Path,
    parent_identity: GcStateDirectoryIdentity,
    archive_path: &Path,
    archive_identity: GcStateFileIdentity,
    expected_bytes: &[u8],
) -> Result<(), BrokerOpError> {
    let file_name = archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| gc_archive_refusal("archive filename is not valid UTF-8"))?;
    let temporary_prefix = format!(".{file_name}-");
    let mut temporary_path = None;
    let mut entries_seen = 0_usize;
    for entry in std::fs::read_dir(parent).map_err(|source| gc_io(parent, source))? {
        let entry = entry.map_err(|source| gc_io(parent, source))?;
        entries_seen = entries_seen.saturating_add(1);
        if !gc_journal_archive_directory_scan_has_budget(entries_seen) {
            return Err(gc_archive_refusal(
                "journal archive directory has too many entries to inspect safely",
            ));
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&temporary_prefix) {
            continue;
        }
        let Some(snapshot) =
            read_gc_state_file_with_link_limit(&entry.path(), parent, parent_identity, 2)?
        else {
            return Err(gc_archive_refusal(
                "interrupted archive publication link disappeared during recovery",
            ));
        };
        if snapshot.identity != archive_identity || snapshot.bytes != expected_bytes {
            return Err(gc_archive_refusal(
                "archive temporary entry does not match the durable archive file",
            ));
        }
        if temporary_path.replace(entry.path()).is_some() {
            return Err(gc_archive_refusal(
                "multiple temporary links match the interrupted archive publication",
            ));
        }
    }
    let Some(temporary_path) = temporary_path else {
        return Err(gc_archive_refusal(
            "durable archive has an unexpected hard link; refusing to clear the active journal",
        ));
    };
    std::fs::remove_file(&temporary_path).map_err(|source| gc_io(&temporary_path, source))?;
    sync_gc_state_directory(parent, parent_identity)?;
    let Some(archive) = read_gc_state_file(archive_path, parent, parent_identity)? else {
        return Err(gc_archive_refusal(
            "archive disappeared while recovering its temporary link",
        ));
    };
    if archive.identity != archive_identity || archive.bytes != expected_bytes {
        return Err(gc_archive_refusal(
            "archive changed while recovering its temporary link",
        ));
    }
    archive
        .file
        .sync_all()
        .map_err(|source| gc_io(archive_path, source))?;
    sync_gc_state_directory(parent, parent_identity)
}

#[cfg(not(unix))]
fn remove_interrupted_gc_archive_link(
    _parent: &Path,
    _parent_identity: GcStateDirectoryIdentity,
    _archive_path: &Path,
    _archive_identity: GcStateFileIdentity,
    _expected_bytes: &[u8],
) -> Result<(), BrokerOpError> {
    Err(gc_archive_refusal(
        "safe no-follow journal archival is supported only on Unix",
    ))
}

#[cfg(unix)]
fn archive_gc_journal_bytes(
    parent: &Path,
    parent_identity: GcStateDirectoryIdentity,
    archive_path: &Path,
    bytes: &[u8],
) -> Result<(), BrokerOpError> {
    use std::os::unix::fs::MetadataExt;

    if let Some(existing) =
        read_gc_state_file_with_link_limit(archive_path, parent, parent_identity, 2)?
    {
        if existing.bytes != bytes {
            return Err(gc_archive_refusal(format!(
                "archive path already contains different bytes: {}",
                archive_path.display()
            )));
        }
        let links = existing
            .file
            .metadata()
            .map_err(|source| gc_io(archive_path, source))?
            .nlink();
        let identity = existing.identity;
        if links == 2 {
            drop(existing);
            remove_interrupted_gc_archive_link(
                parent,
                parent_identity,
                archive_path,
                identity,
                bytes,
            )?;
        } else if links == 1 {
            existing
                .file
                .sync_all()
                .map_err(|source| gc_io(archive_path, source))?;
            sync_gc_state_directory(parent, parent_identity)?;
        } else {
            return Err(gc_archive_refusal(
                "archive has an unexpected number of hard links",
            ));
        }
        return Ok(());
    }

    let (archive_count, archive_bytes) = gc_journal_archive_inventory(parent, parent_identity)?;
    if !gc_journal_archive_budget_admits(archive_count, archive_bytes, bytes.len() as u64) {
        return Err(gc_archive_refusal(format!(
            "journal archive budget is full ({} files / {} bytes); inspect an older archive and retire it with `aethyme broker gc forget-abandoned --confirm <archived-plan-digest>`",
            GC_JOURNAL_ARCHIVE_MAX_COUNT, GC_JOURNAL_ARCHIVE_MAX_BYTES
        )));
    }

    let publish = crate::atomic_file::with_synced_temporary(archive_path, bytes, |temporary| {
        if gc_state_directory_identity(parent).ok() != Some(parent_identity) {
            return Err(std::io::Error::other(
                "GC state directory identity changed before archive publication",
            ));
        }
        std::fs::hard_link(temporary, archive_path)
    });
    match publish {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let Some(existing) =
                read_gc_state_file_with_link_limit(archive_path, parent, parent_identity, 2)?
            else {
                return Err(gc_archive_refusal(
                    "archive path appeared and disappeared during publication",
                ));
            };
            if existing.bytes != bytes {
                return Err(gc_archive_refusal(format!(
                    "archive path already contains different bytes: {}",
                    archive_path.display()
                )));
            }
            let links = existing
                .file
                .metadata()
                .map_err(|source| gc_io(archive_path, source))?
                .nlink();
            let identity = existing.identity;
            if links == 2 {
                drop(existing);
                remove_interrupted_gc_archive_link(
                    parent,
                    parent_identity,
                    archive_path,
                    identity,
                    bytes,
                )?;
            } else if links == 1 {
                existing
                    .file
                    .sync_all()
                    .map_err(|source| gc_io(archive_path, source))?;
            } else {
                return Err(gc_archive_refusal(
                    "archive has an unexpected number of hard links",
                ));
            }
        }
        Err(source) => return Err(gc_io(archive_path, source)),
    }

    sync_gc_state_directory(parent, parent_identity)?;
    let Some(archived) = read_gc_state_file(archive_path, parent, parent_identity)? else {
        return Err(gc_archive_refusal(
            "archive disappeared before its durable readback",
        ));
    };
    if archived.bytes != bytes {
        return Err(gc_archive_refusal(
            "archive readback did not match the pending journal bytes",
        ));
    }
    archived
        .file
        .sync_all()
        .map_err(|source| gc_io(archive_path, source))?;
    sync_gc_state_directory(parent, parent_identity)
}

#[cfg(unix)]
fn ensure_gc_journal_archive_directory(
    state_directory: &Path,
    state_identity: GcStateDirectoryIdentity,
) -> Result<(PathBuf, GcStateDirectoryIdentity), BrokerOpError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};

    if gc_state_directory_identity(state_directory)? != state_identity {
        return Err(gc_archive_refusal(
            "state directory identity changed before archive setup",
        ));
    }
    let archive_directory = state_directory.join(GC_JOURNAL_ARCHIVE_DIRECTORY);
    match std::fs::symlink_metadata(&archive_directory) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&archive_directory) {
                Ok(()) => {
                    crate::host_state::protect_host_state_path(&archive_directory, true)
                        .map_err(|source| gc_io(&archive_directory, source))?;
                    sync_gc_state_directory(state_directory, state_identity)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(gc_io(&archive_directory, source)),
            }
        }
        Err(source) => return Err(gc_io(&archive_directory, source)),
    }
    let identity = gc_state_directory_identity(&archive_directory)?;
    let metadata = std::fs::symlink_metadata(&archive_directory)
        .map_err(|source| gc_io(&archive_directory, source))?;
    if metadata.mode() & 0o077 != 0 {
        return Err(gc_archive_refusal(format!(
            "journal archive directory is not private: {}",
            archive_directory.display()
        )));
    }
    if gc_state_directory_identity(state_directory)? != state_identity
        || gc_state_directory_identity(&archive_directory)? != identity
    {
        return Err(gc_archive_refusal(
            "journal archive directory identity changed during setup",
        ));
    }
    Ok((archive_directory, identity))
}

#[cfg(not(unix))]
fn ensure_gc_journal_archive_directory(
    _state_directory: &Path,
    _state_identity: GcStateDirectoryIdentity,
) -> Result<(PathBuf, GcStateDirectoryIdentity), BrokerOpError> {
    Err(gc_archive_refusal(
        "safe no-follow journal archival is supported only on Unix",
    ))
}

#[cfg(unix)]
fn existing_gc_journal_archive_directory(
    state_directory: &Path,
    state_identity: GcStateDirectoryIdentity,
) -> Result<Option<(PathBuf, GcStateDirectoryIdentity)>, BrokerOpError> {
    use std::os::unix::fs::MetadataExt;

    if gc_state_directory_identity(state_directory)? != state_identity {
        return Err(gc_archive_refusal(
            "state directory identity changed before archive inspection",
        ));
    }
    let archive_directory = state_directory.join(GC_JOURNAL_ARCHIVE_DIRECTORY);
    let metadata = match std::fs::symlink_metadata(&archive_directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(gc_io(&archive_directory, source)),
    };
    let identity = gc_state_directory_identity(&archive_directory)?;
    if metadata.mode() & 0o077 != 0
        || gc_state_directory_identity(state_directory)? != state_identity
        || gc_state_directory_identity(&archive_directory)? != identity
    {
        return Err(gc_archive_refusal(format!(
            "journal archive directory is not private or changed during inspection: {}",
            archive_directory.display()
        )));
    }
    Ok(Some((archive_directory, identity)))
}

#[cfg(not(unix))]
fn existing_gc_journal_archive_directory(
    _state_directory: &Path,
    _state_identity: GcStateDirectoryIdentity,
) -> Result<Option<(PathBuf, GcStateDirectoryIdentity)>, BrokerOpError> {
    Err(gc_archive_refusal(
        "safe no-follow journal archival is supported only on Unix",
    ))
}

#[cfg(not(unix))]
fn archive_gc_journal_bytes(
    _parent: &Path,
    _parent_identity: GcStateDirectoryIdentity,
    _archive_path: &Path,
    _bytes: &[u8],
) -> Result<(), BrokerOpError> {
    Err(gc_archive_refusal(
        "safe no-follow journal archival is supported only on Unix",
    ))
}

fn gc_journal_archive_digest(name: &str) -> Option<&str> {
    let digest = name
        .strip_prefix(GC_JOURNAL_ARCHIVE_PREFIX)?
        .strip_suffix(GC_JOURNAL_ARCHIVE_SUFFIX)?;
    (digest.len() == 64
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        && digest.bytes().all(|byte| !byte.is_ascii_uppercase()))
    .then_some(digest)
}

fn gc_journal_archive_temporary_name(name: &str, archive_name: &str) -> bool {
    let Some(suffix) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_prefix(archive_name))
        .and_then(|name| name.strip_prefix('-'))
    else {
        return false;
    };
    (6..=64).contains(&suffix.len()) && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// Explicitly retire only the private random temporary names emitted by
/// `with_synced_temporary` for this exact archive. A temporary file was never
/// published as an archive, so deleting it cannot revoke a journal; the
/// active journal remains available for a fresh `abandon` retry.
fn remove_gc_journal_archive_temporaries(
    directory: &Path,
    directory_identity: GcStateDirectoryIdentity,
    archive_name: &str,
    confirmed_digest: &str,
) -> Result<u64, BrokerOpError> {
    struct TemporarySnapshot {
        path: PathBuf,
        identity: GcStateFileIdentity,
        bytes: Vec<u8>,
    }

    if gc_state_directory_identity(directory)? != directory_identity {
        return Err(gc_archive_refusal(
            "journal archive directory identity changed before temporary-file inspection",
        ));
    }
    let mut temporaries = Vec::new();
    let mut bytes_total = 0_u64;
    let mut entries_seen = 0_usize;
    for entry in std::fs::read_dir(directory).map_err(|source| gc_io(directory, source))? {
        let entry = entry.map_err(|source| gc_io(directory, source))?;
        entries_seen = entries_seen.saturating_add(1);
        if !gc_journal_archive_directory_scan_has_budget(entries_seen) {
            return Err(gc_archive_refusal(
                "journal archive directory has too many entries to inspect safely",
            ));
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let temporary_prefix = format!(".{archive_name}-");
        if !name.starts_with(&temporary_prefix) {
            continue;
        }
        if !gc_journal_archive_temporary_name(name, archive_name) {
            return Err(gc_archive_refusal(format!(
                "journal archive directory contains an unrecognized temporary entry: {name:?}"
            )));
        }
        if temporaries.len() >= GC_JOURNAL_ARCHIVE_MAX_COUNT {
            return Err(gc_archive_refusal(
                "too many matching temporary archive files to retire safely",
            ));
        }
        let path = entry.path();
        let Some(snapshot) = read_gc_state_file(&path, directory, directory_identity)? else {
            return Err(gc_archive_refusal(format!(
                "temporary archive file disappeared during inspection: {name}"
            )));
        };
        bytes_total = bytes_total
            .checked_add(snapshot.bytes.len() as u64)
            .ok_or_else(|| gc_archive_refusal("temporary archive byte count overflowed"))?;
        if bytes_total > GC_JOURNAL_ARCHIVE_MAX_BYTES {
            return Err(gc_archive_refusal(
                "matching temporary archive files exceed the configured byte budget",
            ));
        }
        // A fully written temp must carry the digest encoded by its exact
        // archive name. A truncated temp from a crash may not parse yet.
        if let Ok(journal) = serde_json::from_slice::<GcJournal>(&snapshot.bytes)
            && (!gc_journal_schema_is_revocable(journal.schema_version)
                || !journal.digest.eq_ignore_ascii_case(confirmed_digest))
        {
            return Err(gc_archive_refusal(format!(
                "temporary archive file {name} contains a different journal"
            )));
        }
        temporaries.push(TemporarySnapshot {
            path,
            identity: snapshot.identity,
            bytes: snapshot.bytes,
        });
    }
    if gc_state_directory_identity(directory)? != directory_identity {
        return Err(gc_archive_refusal(
            "journal archive directory identity changed after temporary-file inspection",
        ));
    }

    for temporary in &temporaries {
        let Some(current) = read_gc_state_file(&temporary.path, directory, directory_identity)?
        else {
            return Err(gc_archive_refusal(
                "temporary archive file disappeared before it could be retired",
            ));
        };
        if current.identity != temporary.identity || current.bytes != temporary.bytes {
            return Err(gc_archive_refusal(
                "temporary archive file changed before it could be retired",
            ));
        }
    }
    for temporary in &temporaries {
        std::fs::remove_file(&temporary.path).map_err(|source| gc_io(&temporary.path, source))?;
    }
    if !temporaries.is_empty() {
        sync_gc_state_directory(directory, directory_identity)?;
    }
    Ok(bytes_total)
}

fn gc_journal_archive_inventory(
    directory: &Path,
    directory_identity: GcStateDirectoryIdentity,
) -> Result<(usize, u64), BrokerOpError> {
    if gc_state_directory_identity(directory)? != directory_identity {
        return Err(gc_archive_refusal(
            "journal archive directory identity changed during inventory",
        ));
    }
    let entries = std::fs::read_dir(directory).map_err(|source| gc_io(directory, source))?;
    let mut count = 0_usize;
    let mut bytes = 0_u64;
    let mut entries_seen = 0_usize;
    for entry in entries {
        let entry = entry.map_err(|source| gc_io(directory, source))?;
        entries_seen = entries_seen.saturating_add(1);
        if !gc_journal_archive_directory_scan_has_budget(entries_seen) {
            return Err(gc_archive_refusal(
                "journal archive directory has too many entries to inspect safely",
            ));
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(gc_archive_refusal(
                "journal archive directory contains a non-UTF-8 entry",
            ));
        };
        let Some(expected_digest) = gc_journal_archive_digest(name) else {
            return Err(gc_archive_refusal(format!(
                "journal archive directory contains an unrecognized entry: {name:?}"
            )));
        };
        let Some(snapshot) = read_gc_state_file(&entry.path(), directory, directory_identity)?
        else {
            return Err(gc_archive_refusal(format!(
                "journal archive disappeared during inventory: {name}"
            )));
        };
        let journal: GcJournal = serde_json::from_slice(&snapshot.bytes).map_err(|error| {
            gc_archive_refusal(format!("journal archive {name} is not valid JSON: {error}"))
        })?;
        if !gc_journal_schema_is_revocable(journal.schema_version)
            || !journal.digest.eq_ignore_ascii_case(expected_digest)
        {
            return Err(gc_archive_refusal(format!(
                "journal archive {name} does not match its name or a supported schema"
            )));
        }
        count = count.saturating_add(1);
        bytes = bytes
            .checked_add(snapshot.bytes.len() as u64)
            .ok_or_else(|| gc_archive_refusal("journal archive byte count overflowed"))?;
        if count > GC_JOURNAL_ARCHIVE_MAX_COUNT || bytes > GC_JOURNAL_ARCHIVE_MAX_BYTES {
            return Err(gc_archive_refusal(
                "existing journal archives exceed the configured count or byte budget",
            ));
        }
    }
    if gc_state_directory_identity(directory)? != directory_identity {
        return Err(gc_archive_refusal(
            "journal archive directory identity changed after inventory",
        ));
    }
    Ok((count, bytes))
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

fn scoped_artifact_sweep_key(namespace: &str, legacy_key: &str) -> String {
    let suffix = legacy_key
        .strip_prefix(ARTIFACT_SWEEP_META_PREFIX)
        .unwrap_or(legacy_key);
    format!("gc.artifact_sweep.v2.{namespace}.{suffix}")
}

fn bounded_legacy_artifact_sweep_meta_get(
    store: &crate::BrokerStore,
    key: &str,
    max_bytes: usize,
) -> Result<Option<String>, BrokerOpError> {
    let value = store
        .connection()
        .query_row(
            "SELECT typeof(value), length(CAST(value AS BLOB)),
                    CASE WHEN typeof(value) = 'text'
                              AND length(CAST(value AS BLOB)) <= ?2
                         THEN CAST(value AS BLOB)
                         ELSE NULL
                    END
             FROM meta WHERE key = ?1",
            rusqlite::params![key, max_bytes as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                ))
            },
        )
        .optional()?;
    match value {
        None => Ok(None),
        Some((kind, Some(length), Some(bytes)))
            if kind == "text"
                && length >= 0
                && length as usize <= max_bytes
                && bytes.len() == length as usize =>
        {
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| BrokerOpError::InvalidStorageContext {
                    reason: format!("legacy artifact-sweep field {key} is not UTF-8")
                        .into_boxed_str(),
                })
        }
        Some(_) => Err(BrokerOpError::InvalidStorageContext {
            reason: format!(
                "legacy artifact-sweep field {key} is non-text or exceeds its {max_bytes}-byte inspection bound"
            )
            .into_boxed_str(),
        }),
    }
}

impl Broker {
    fn ensure_artifact_sweep_namespace(&mut self) -> Result<String, BrokerOpError> {
        if let Some(namespace) = self.artifact_sweep_namespace.get() {
            return Ok(namespace.clone());
        }
        let namespace = self.storage_context().artifact_sweep_namespace()?;
        let registration = self.store().register_artifact_sweep_namespace(&namespace)?;
        match registration {
            crate::broker::ArtifactSweepNamespaceRegistration::Registered => {
                Ok(self
                    .artifact_sweep_namespace
                    .get_or_init(|| namespace.clone())
                    .clone())
            }
            crate::broker::ArtifactSweepNamespaceRegistration::AtCapacity => {
                Err(BrokerOpError::InvalidStorageContext {
                    reason: "artifact-sweep context registry is full (64 namespaces); no sweep state was read or changed".into(),
                })
            }
            crate::broker::ArtifactSweepNamespaceRegistration::InvalidRegistry => {
                Err(BrokerOpError::InvalidStorageContext {
                    reason: "artifact-sweep context registry is invalid or exceeds its 8 KiB bound; its raw value was preserved and maintenance refused".into(),
                })
            }
        }
    }

    fn artifact_sweep_namespace(&self) -> Result<&str, BrokerOpError> {
        self.artifact_sweep_namespace
            .get()
            .map(String::as_str)
            .ok_or_else(|| BrokerOpError::InvalidStorageContext {
                reason: "artifact-sweep context namespace was not registered before state access"
                    .into(),
            })
    }

    fn artifact_sweep_key(&self, legacy_key: &str) -> Result<String, BrokerOpError> {
        Ok(scoped_artifact_sweep_key(
            self.artifact_sweep_namespace()?,
            legacy_key,
        ))
    }

    fn artifact_sweep_meta_get(
        &mut self,
        legacy_key: &str,
    ) -> Result<Option<String>, BrokerOpError> {
        self.ensure_artifact_sweep_namespace()?;
        let key = self.artifact_sweep_key(legacy_key)?;
        Ok(self.store().meta_get(&key)?)
    }

    fn artifact_sweep_meta_set(
        &mut self,
        legacy_key: &str,
        value: &str,
    ) -> Result<(), BrokerOpError> {
        self.ensure_artifact_sweep_namespace()?;
        let key = self.artifact_sweep_key(legacy_key)?;
        self.store().meta_set(&key, value)?;
        Ok(())
    }

    /// Inspect the nine pre-v2 sweep keys within their write bounds. Any
    /// malformed, oversized or non-text value remains untouched and is
    /// surfaced as unresolved legacy state.
    fn legacy_artifact_sweep_state(&mut self) -> ArtifactSweepLegacyUnscopedState {
        let inspect = (|| -> Result<bool, BrokerOpError> {
            let read_scalar = |broker: &mut Self, key| {
                bounded_legacy_artifact_sweep_meta_get(
                    broker.store(),
                    key,
                    ARTIFACT_SWEEP_LEGACY_SCALAR_MAX_BYTES,
                )
            };
            let stamp = read_scalar(self, ARTIFACT_SWEEP_STAMP_KEY)?;
            let cursor = read_scalar(self, ARTIFACT_SWEEP_CURSOR_KEY)?;
            let progress = read_scalar(self, ARTIFACT_SWEEP_PROGRESS_KEY)?;
            let interrupted = bounded_legacy_artifact_sweep_meta_get(
                self.store(),
                ARTIFACT_SWEEP_INTERRUPTED_KEY,
                ARTIFACT_SWEEP_INTERRUPTED_VALUE_MAX_BYTES,
            )?;
            let blocked = bounded_legacy_artifact_sweep_meta_get(
                self.store(),
                ARTIFACT_SWEEP_BLOCKED_PATHS_KEY,
                ARTIFACT_SWEEP_BLOCKED_PATHS_VALUE_MAX_BYTES,
            )?;
            let requested = read_scalar(self, ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)?;
            let completed = read_scalar(self, ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)?;
            let retry_after = read_scalar(self, ARTIFACT_SWEEP_RETRY_AFTER_KEY)?;
            let _worker_status = read_scalar(self, ARTIFACT_SWEEP_WORKER_STATUS_KEY)?;

            let parse_nonnegative = |value: Option<String>| -> Result<i64, ()> {
                match value.as_deref().filter(|value| !value.is_empty()) {
                    None => Ok(0),
                    Some(value) => value
                        .parse::<i64>()
                        .ok()
                        .filter(|value| *value >= 0)
                        .ok_or(()),
                }
            };
            let stamp =
                parse_nonnegative(stamp).map_err(|()| BrokerOpError::InvalidStorageContext {
                    reason: "legacy artifact-sweep stamp is malformed".into(),
                })?;
            let cursor =
                parse_nonnegative(cursor).map_err(|()| BrokerOpError::InvalidStorageContext {
                    reason: "legacy artifact-sweep cursor is malformed".into(),
                })?;
            let progress =
                parse_nonnegative(progress).map_err(|()| BrokerOpError::InvalidStorageContext {
                    reason: "legacy artifact-sweep progress is malformed".into(),
                })?;
            let requested = parse_nonnegative(requested).map_err(|()| {
                BrokerOpError::InvalidStorageContext {
                    reason: "legacy artifact-sweep request is malformed".into(),
                }
            })?;
            let completed = parse_nonnegative(completed).map_err(|()| {
                BrokerOpError::InvalidStorageContext {
                    reason: "legacy artifact-sweep completion is malformed".into(),
                }
            })?;
            let retry_after = parse_nonnegative(retry_after).map_err(|()| {
                BrokerOpError::InvalidStorageContext {
                    reason: "legacy artifact-sweep retry time is malformed".into(),
                }
            })?;
            if completed > requested {
                return Err(BrokerOpError::InvalidStorageContext {
                    reason: "legacy artifact-sweep request/completion state is inconsistent".into(),
                });
            }
            let interrupted_pending = match interrupted.as_deref() {
                None | Some("") => false,
                Some(value) => !decode_interrupted_paths(Some(value))
                    .map_err(|()| BrokerOpError::InvalidStorageContext {
                        reason: "legacy artifact-sweep interrupted-path state is malformed".into(),
                    })?
                    .is_empty(),
            };
            let blocked_pending = match blocked.as_deref() {
                None | Some("") => false,
                Some(value) => {
                    let paths =
                        serde_json::from_str::<Vec<serde_json::Value>>(value).map_err(|_| {
                            BrokerOpError::InvalidStorageContext {
                                reason: "legacy artifact-sweep blocked-path state is malformed"
                                    .into(),
                            }
                        })?;
                    !paths.is_empty()
                }
            };
            let pending = cursor > 0
                || progress > 0
                || interrupted_pending
                || blocked_pending
                || requested > completed
                || retry_after > now_ms();
            let _last_stamp = stamp;
            Ok(pending)
        })();
        match inspect {
            Ok(true) | Err(_) => ArtifactSweepLegacyUnscopedState::UnknownPending,
            Ok(false) => ArtifactSweepLegacyUnscopedState::Clear,
        }
    }

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
        age_anchor_ms: i64,
        policy: &RetentionPolicy,
        cleanup: &[crate::CleanupWorktreePlan],
        sessions: &BTreeMap<i64, crate::Session>,
        excluded: ArtifactExclusions<'_>,
        size_scan: crate::SizeScan,
    ) -> ArtifactCandidates {
        let budget = (!size_scan.measures()).then_some(HEALTH_CHECK_ARTIFACT_SCAN_BUDGET);
        self.artifact_candidates_within(
            age_anchor_ms,
            policy,
            cleanup,
            sessions,
            excluded,
            size_scan,
            budget,
            None,
        )
    }

    /// [`Self::artifact_candidates`] with an explicit scan budget, `None` for
    /// a complete scan. Worktrees left once the budget is spent are counted,
    /// not scanned.
    #[allow(clippy::too_many_arguments)]
    fn artifact_candidates_within(
        &self,
        age_anchor_ms: i64,
        policy: &RetentionPolicy,
        cleanup: &[crate::CleanupWorktreePlan],
        sessions: &BTreeMap<i64, crate::Session>,
        excluded: ArtifactExclusions<'_>,
        size_scan: crate::SizeScan,
        budget: Option<std::time::Duration>,
        deadline: Option<Instant>,
    ) -> ArtifactCandidates {
        let ArtifactExclusions {
            already_removed,
            live_worktrees,
        } = excluded;
        let started = std::time::Instant::now();
        let scan_deadline = deadline.or_else(|| budget.map(|budget| started + budget));
        let _git_deadline = scan_deadline.map(crate::git::limit_git_until);
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
            let idle_days = days_between(age_anchor_ms, closed_at);
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
            if deadline_expired(scan_deadline) {
                not_scanned += 1;
                continue;
            }
            let Ok(checkout) = GitRepo::discover(&root) else {
                if deadline_expired(scan_deadline) {
                    not_scanned += 1;
                }
                continue;
            };
            // Ignored directories outside the catalog are only reported with
            // their size, so a pass that measures nothing skips finding them:
            // that search costs one `git check-ignore` per directory walked.
            let (artifact_scan, discovery_complete) = if size_scan.measures()
                && let Some(deadline) = scan_deadline
            {
                collect_artifact_scan_until(
                    &root,
                    &policy.artefact_directories,
                    &checkout,
                    deadline,
                    ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET,
                )
            } else if size_scan.measures() {
                (
                    collect_artifact_scan(&root, &policy.artefact_directories, &checkout),
                    true,
                )
            } else if let Some(deadline) = scan_deadline {
                let discovery = collect_artifact_dirs_bounded(
                    &root,
                    &policy.artefact_directories,
                    deadline,
                    ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET,
                );
                (
                    ArtifactScan {
                        classified: discovery.classified,
                        declined: Vec::new(),
                    },
                    discovery.complete,
                )
            } else {
                let mut classified = Vec::new();
                collect_artifact_dirs_with_extras(
                    &root,
                    &root,
                    0,
                    &policy.artefact_directories,
                    &mut classified,
                );
                (
                    ArtifactScan {
                        classified,
                        declined: Vec::new(),
                    },
                    true,
                )
            };
            if !discovery_complete {
                not_scanned += 1;
                continue;
            }
            let candidates_before_worktree = candidates.len();
            let mut worktree_complete = true;
            for dir in artifact_scan.classified {
                if deadline_expired(scan_deadline) {
                    worktree_complete = false;
                    break;
                }
                let Some(relative) = repo_relative(&root, &dir) else {
                    continue;
                };
                let ignored = match scan_deadline {
                    Some(_) => match checkout.path_is_ignored_checked(&relative) {
                        Ok(ignored) => ignored,
                        Err(_) => {
                            worktree_complete = false;
                            break;
                        }
                    },
                    None => checkout.path_is_ignored(&relative),
                };
                if deadline_expired(scan_deadline) {
                    worktree_complete = false;
                    break;
                }
                if !ignored {
                    continue;
                }
                // Discovery is `read_dir` plus one `check-ignore`; sizing is
                // the walk. A recorded-size pass keeps the candidate and
                // reports zero bytes with an unknown inode count -- dropping
                // it would understate the count (#176). A measured directory
                // holding no bytes and nothing but itself (an empty `target/`)
                // frees nothing worth a candidate.
                let usage = if size_scan.measures() {
                    let measured = match scan_deadline {
                        Some(deadline) => {
                            crate::disk_headroom::directory_usage_bounded(&dir, deadline)
                        }
                        None => {
                            crate::disk_headroom::directory_usage_without_following_links(&dir).ok()
                        }
                    };
                    match measured {
                        Some(usage) if usage.bytes == 0 && usage.inodes <= 1 => continue,
                        Some(usage) => Some(usage),
                        None if scan_deadline.is_some() => {
                            worktree_complete = false;
                            break;
                        }
                        None => continue,
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
            if !worktree_complete {
                candidates.truncate(candidates_before_worktree);
                not_scanned += 1;
                continue;
            }
            if size_scan.measures() {
                let (reported_declined, declined_complete) = declined_artifacts(
                    item.session_id,
                    &item.worktree_path,
                    &root,
                    artifact_scan.declined,
                    &policy.artefact_directories,
                    scan_deadline,
                );
                if !declined_complete {
                    candidates.truncate(candidates_before_worktree);
                    not_scanned += 1;
                    continue;
                }
                declined.extend(reported_declined);
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
        age_anchor_ms: i64,
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
        let archive_scan = if let Some(deadline) = deadline {
            let root = self.recovery_archive_root()?;
            if std::time::Instant::now() >= deadline {
                return Ok((Vec::new(), None, false));
            }
            match std::fs::symlink_metadata(&root) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok((Vec::new(), None, true));
                }
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                _ => return Ok((Vec::new(), None, false)),
            }
            if std::time::Instant::now() >= deadline {
                return Ok((Vec::new(), None, false));
            }
            match std::fs::read_dir(&root) {
                Ok(mut entries) => match entries.next() {
                    None => {
                        return Ok((Vec::new(), None, true));
                    }
                    Some(Ok(_)) => {
                        // Verifying a recovery archive can hash a bundle up to
                        // 1 GiB. Keep the default plan bounded by deferring the
                        // entire archive set; `--full-scan` performs the
                        // exhaustive ownership and integrity checks.
                        return Ok((Vec::new(), None, false));
                    }
                    Some(Err(_)) => return Ok((Vec::new(), None, false)),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok((Vec::new(), None, true));
                }
                Err(_) => return Ok((Vec::new(), None, false)),
            }
        } else {
            self.recovery_archives()?
        };
        let crate::cleanup_resolve::RecoveryArchiveScan {
            root,
            owned,
            unowned,
        } = archive_scan;
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
                recovery_archive_disposition(self, archive, policy, age_anchor_ms)
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
        age_anchor_ms: i64,
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
        let database = self.host_operation_database_path()?;
        let inventory = crate::worktree_registry::inventory(&database, deadline);
        let mut complete = inventory.complete;
        for reason in inventory.issues {
            blockers.push(GcBlocker {
                kind: "worktree_root_history_uninspected".into(),
                id: None,
                reason,
            });
        }
        let records: std::collections::BTreeMap<_, _> = inventory
            .roots
            .into_iter()
            .map(|record| (record.root.clone(), record))
            .collect();
        let mut roots: std::collections::BTreeSet<_> = records.keys().cloned().collect();
        roots.extend(plan.preferred_root);
        roots.extend(plan.host_state_fallback_root);
        roots.insert(plan.legacy_fallback_root);
        if let Some(container) = plan.root_container {
            match std::fs::read_dir(container) {
                Ok(entries) => {
                    for (index, entry) in entries.enumerate() {
                        if index >= 8192
                            || deadline
                                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
                        {
                            complete = false;
                            break;
                        }
                        match entry {
                            Ok(entry) => {
                                roots.insert(entry.path());
                            }
                            Err(error) => {
                                complete = false;
                                blockers.push(GcBlocker {
                                    kind: "worktree_root_history_uninspected".into(),
                                    id: None,
                                    reason: error.to_string(),
                                });
                            }
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    complete = false;
                    blockers.push(GcBlocker {
                        kind: "worktree_root_history_uninspected".into(),
                        id: None,
                        reason: error.to_string(),
                    });
                }
            }
        }
        let mut candidates = Vec::new();
        let mut inspected_roots = std::collections::BTreeSet::new();
        for root in roots {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                complete = false;
                break;
            }
            if !is_real_directory(&root) {
                continue;
            }
            // Registry records use canonical roots. Container discovery may
            // reach the same directory through an ancestor alias (for example
            // macOS /var and /private/var); compare the same identity once.
            // Reject a symlink leaf above before resolving ancestor aliases.
            let root = match std::fs::canonicalize(&root) {
                Ok(root) => root,
                Err(error) => {
                    complete = false;
                    blockers.push(GcBlocker {
                        kind: "worktree_root_history_uninspected".into(),
                        id: None,
                        reason: format!("{}: {error}", root.display()),
                    });
                    continue;
                }
            };
            if !inspected_roots.insert(root.clone()) {
                continue;
            }
            let marker = match crate::worktree_registry::read_marker(&root) {
                Ok(marker) => marker,
                Err(error) => {
                    complete = false;
                    blockers.push(GcBlocker {
                        kind: "unmarked_worktree_root".into(),
                        id: None,
                        reason: format!(
                            "{}: {error}; never swept without ownership evidence",
                            root.display()
                        ),
                    });
                    continue;
                }
            };
            // An existing origin always prevents orphan deletion. Missing
            // legacy origins cannot be upgraded into mounted-volume proof.
            match std::fs::symlink_metadata(&marker.repository_root) {
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    complete = false;
                    blockers.push(GcBlocker {
                        kind: "worktree_origin_uninspected".into(),
                        id: None,
                        reason: error.to_string(),
                    });
                    continue;
                }
            }
            let fingerprint = records
                .get(&root)
                .ok_or_else(|| "root has no saved origin filesystem identity".to_string())
                .and_then(|record| {
                    crate::worktree_registry::ownership_fingerprint(record)
                        .map_err(|error| error.to_string())
                });
            let fingerprint = match fingerprint {
                Ok(fingerprint) => fingerprint,
                Err(reason) => {
                    complete = false;
                    blockers.push(GcBlocker {
                        kind: "worktree_origin_uninspected".into(),
                        id: None,
                        reason: format!(
                            "{}: {reason}; retain until independently adopted",
                            root.display()
                        ),
                    });
                    continue;
                }
            };
            let age_days = std::fs::metadata(&root)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|since| days_between(age_anchor_ms, since.as_millis() as i64))
                .unwrap_or(0);
            if age_days < policy.orphan_worktree_roots_days {
                blockers.push(GcBlocker {
                    kind: "orphan_grace".into(),
                    id: None,
                    reason: format!(
                        "orphaned root {} is younger than the {} day grace period",
                        root.display(),
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
                ownership_fingerprint: Some(fingerprint),
                reason: "owning repository no longer exists on its recorded mounted filesystem"
                    .into(),
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

    /// The default operator-facing plan is cooperative and deadline-bound.
    /// `full_scan` selects the exhaustive compatibility path explicitly.
    pub(crate) fn gc_plan_for_cli(
        &mut self,
        include_active_gate_cache: bool,
        full_scan: bool,
    ) -> Result<GcPlan, BrokerOpError> {
        if full_scan {
            self.gc_plan_scanned(crate::SizeScan::Measure, include_active_gate_cache)
        } else {
            self.gc_plan_scanned_until(
                crate::SizeScan::Measure,
                include_active_gate_cache,
                Some(Instant::now() + MANUAL_GC_PLAN_BUDGET),
            )
        }
    }

    /// Retire an ineligible frozen journal after the operator confirms its
    /// exact digest. This archives only the journal bytes: it never resumes,
    /// replans, or removes any retained row, file, worktree, or artifact.
    pub(crate) fn gc_abandon_pending(
        &mut self,
        confirm: &str,
    ) -> Result<GcJournalAbandonReport, BrokerOpError> {
        if !is_sha256_digest(confirm) {
            return Err(BrokerOpError::GcConfirmationNotSha256);
        }

        let main_root = self.main_root().to_path_buf();
        let _lock = GcLock::acquire(&main_root)?;
        let state_directory = main_root.join(".aethyme");
        let directory_identity = gc_state_directory_identity(&state_directory)?;
        let journal_path = state_directory.join("gc-journal.json");
        let Some(pending) =
            read_gc_state_file(&journal_path, &state_directory, directory_identity)?
        else {
            return Err(gc_archive_refusal(
                "no pending journal exists; review a fresh `aethyme broker gc plan`",
            ));
        };
        let journal: GcJournal = serde_json::from_slice(&pending.bytes).map_err(|error| {
            BrokerOpError::InvalidGcJournal {
                reason: error.to_string(),
            }
        })?;
        if !gc_journal_schema_is_revocable(journal.schema_version) {
            return Err(BrokerOpError::InvalidGcJournal {
                reason: format!(
                    "schema {} is unknown and cannot be safely abandoned (supported schemas: 2, {} and {})",
                    journal.schema_version, GC_LEGACY_PLAN_SCHEMA_VERSION, GC_PLAN_SCHEMA_VERSION,
                ),
            });
        }
        if !is_sha256_digest(&journal.digest) {
            return Err(BrokerOpError::InvalidGcJournal {
                reason: "pending journal digest is not a full SHA-256 value".into(),
            });
        }
        if !journal.digest.eq_ignore_ascii_case(confirm) {
            return Err(BrokerOpError::GcAbandonConfirmationMismatch {
                expected: journal.digest,
                actual: confirm.to_owned(),
            });
        }

        let archive_name = format!(
            "{GC_JOURNAL_ARCHIVE_PREFIX}{}{GC_JOURNAL_ARCHIVE_SUFFIX}",
            journal.digest.to_ascii_lowercase()
        );
        let (archive_directory, archive_directory_identity) =
            ensure_gc_journal_archive_directory(&state_directory, directory_identity)?;
        let archive_path = archive_directory.join(&archive_name);
        let bytes_archived = pending.bytes.len() as u64;
        archive_gc_journal_bytes(
            &archive_directory,
            archive_directory_identity,
            &archive_path,
            &pending.bytes,
        )?;

        if gc_state_directory_identity(&state_directory)? != directory_identity {
            return Err(gc_archive_refusal(
                "state directory identity changed before clearing the active journal",
            ));
        }
        let Some(current) =
            read_gc_state_file(&journal_path, &state_directory, directory_identity)?
        else {
            return Err(gc_archive_refusal(
                "active journal disappeared after it was archived",
            ));
        };
        if current.identity != pending.identity || current.bytes != pending.bytes {
            return Err(gc_archive_refusal(
                "active journal changed after the archive was written; it was left in place",
            ));
        }
        drop(current);
        drop(pending);
        std::fs::remove_file(&journal_path).map_err(|source| gc_io(&journal_path, source))?;
        sync_gc_state_directory(&state_directory, directory_identity)?;

        Ok(GcJournalAbandonReport {
            digest: journal.digest,
            archive_path: format!(".aethyme/{GC_JOURNAL_ARCHIVE_DIRECTORY}/{archive_name}"),
            bytes_archived,
        })
    }

    /// Remove only a previously abandoned journal or its known atomic-write
    /// temporary files after the operator confirms its plan digest. This
    /// never touches GC candidates or changes an active journal.
    pub(crate) fn gc_forget_abandoned(
        &mut self,
        confirm: &str,
    ) -> Result<GcJournalForgetReport, BrokerOpError> {
        if !is_sha256_digest(confirm) {
            return Err(BrokerOpError::GcConfirmationNotSha256);
        }

        let main_root = self.main_root().to_path_buf();
        let _lock = GcLock::acquire(&main_root)?;
        let state_directory = main_root.join(".aethyme");
        let state_identity = gc_state_directory_identity(&state_directory)?;
        let Some((archive_directory, archive_directory_identity)) =
            existing_gc_journal_archive_directory(&state_directory, state_identity)?
        else {
            return Err(gc_archive_refusal(
                "no matching abandoned journal exists; nothing was forgotten",
            ));
        };
        let archive_name = format!(
            "{GC_JOURNAL_ARCHIVE_PREFIX}{}{GC_JOURNAL_ARCHIVE_SUFFIX}",
            confirm.to_ascii_lowercase()
        );
        let archive_path = archive_directory.join(&archive_name);
        let archive = read_gc_state_file_with_link_limit(
            &archive_path,
            &archive_directory,
            archive_directory_identity,
            2,
        )?;
        let archive_journal = if let Some(archive) = &archive {
            let journal: GcJournal = serde_json::from_slice(&archive.bytes).map_err(|error| {
                gc_archive_refusal(format!("archived journal is not valid JSON: {error}"))
            })?;
            if !gc_journal_schema_is_revocable(journal.schema_version)
                || !journal.digest.eq_ignore_ascii_case(confirm)
            {
                return Err(gc_archive_refusal(
                    "archived journal schema or digest does not match the confirmation",
                ));
            }
            Some(journal)
        } else {
            None
        };

        let active_path = state_directory.join("gc-journal.json");
        if let Some(active) = read_gc_state_file(&active_path, &state_directory, state_identity)? {
            let active_journal: GcJournal =
                serde_json::from_slice(&active.bytes).map_err(|error| {
                    gc_archive_refusal(format!(
                        "active journal cannot be checked before forgetting an archive: {error}"
                    ))
                })?;
            if !gc_journal_schema_is_revocable(active_journal.schema_version) {
                return Err(gc_archive_refusal(
                    "active journal has an unknown schema; no archive was forgotten",
                ));
            }
            if archive
                .as_ref()
                .is_some_and(|archive| active.bytes == archive.bytes)
            {
                return Err(gc_archive_refusal(
                    "the active journal still needs this exact archive for crash recovery; retry `gc abandon` first",
                ));
            }
        }

        let mut bytes_forgotten = 0_u64;
        let temporary_bytes = if let Some(archive) = archive {
            let links = gc_state_file_link_count(
                &archive
                    .file
                    .metadata()
                    .map_err(|source| gc_io(&archive_path, source))?,
            );
            if links == 2 {
                remove_interrupted_gc_archive_link(
                    &archive_directory,
                    archive_directory_identity,
                    &archive_path,
                    archive.identity,
                    &archive.bytes,
                )?;
            } else if links != 1 {
                return Err(gc_archive_refusal(
                    "archive has an unexpected number of hard links",
                ));
            }
            // Validate and retire all remaining matching temporary entries
            // before unlinking the durable archive. A suspicious temp entry
            // must not make us lose the archive as evidence on refusal.
            let temporary_bytes = remove_gc_journal_archive_temporaries(
                &archive_directory,
                archive_directory_identity,
                &archive_name,
                confirm,
            )?;
            let Some(current) = read_gc_state_file(
                &archive_path,
                &archive_directory,
                archive_directory_identity,
            )?
            else {
                return Err(gc_archive_refusal(
                    "archive disappeared before it could be forgotten",
                ));
            };
            if current.identity != archive.identity || current.bytes != archive.bytes {
                return Err(gc_archive_refusal(
                    "archive changed before it could be forgotten",
                ));
            }
            bytes_forgotten = bytes_forgotten.saturating_add(current.bytes.len() as u64);
            drop(current);
            drop(archive);
            std::fs::remove_file(&archive_path).map_err(|source| gc_io(&archive_path, source))?;
            sync_gc_state_directory(&archive_directory, archive_directory_identity)?;
            temporary_bytes
        } else {
            remove_gc_journal_archive_temporaries(
                &archive_directory,
                archive_directory_identity,
                &archive_name,
                confirm,
            )?
        };
        if archive_journal.is_none() && temporary_bytes == 0 {
            return Err(gc_archive_refusal(
                "no matching abandoned journal or atomic-write temporary exists; nothing was forgotten",
            ));
        }
        bytes_forgotten = bytes_forgotten.saturating_add(temporary_bytes);
        Ok(GcJournalForgetReport {
            digest: archive_journal.map_or_else(|| confirm.to_ascii_lowercase(), |j| j.digest),
            bytes_forgotten,
        })
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
        let _git_deadline = deadline.map(crate::git::limit_git_until);
        // The budget is cooperative: loops and stage boundaries check it, but
        // an in-flight SQLite query or filesystem syscall cannot be cancelled
        // by this deadline and may finish after it.
        let evaluated_at = now_ms();
        let age_anchor_ms = utc_day_floor_ms(evaluated_at);
        let main_root = self.main_root().to_path_buf();
        let retention_config = load_retention_policy_report(&main_root)?;
        let policy = retention_config.policy;
        let retention_config_warnings = retention_config.warnings;
        let exposure_cutoff = cutoff(age_anchor_ms, policy.publication_exposure_days);
        let (cleanup, cleanup_size_records_complete) = match deadline {
            Some(deadline) => self.cleanup_plan_scanned_until(scan, deadline)?,
            None => (self.cleanup_plan_scanned(scan)?, true),
        };
        let mut deferred_checks = Vec::new();
        if !cleanup_size_records_complete {
            deferred_checks.push("cleanup_size_records".into());
        }
        if cleanup.eligibility_not_inspected_count > 0 {
            deferred_checks.push("cleanup_eligibility".into());
        }
        let sessions = if deadline_expired(deadline) {
            deferred_checks.push("closed_sessions".into());
            BTreeMap::new()
        } else {
            self.store()
                .cleaned_sessions()?
                .into_iter()
                .map(|session| (session.id, session))
                .collect::<BTreeMap<_, _>>()
        };
        let mut rows = if deadline_expired(deadline) {
            deferred_checks.push("gc_rows".into());
            Vec::new()
        } else if deadline.is_some() {
            self.store().gc_row_candidates_for_health(
                cutoff(age_anchor_ms, policy.terminal_events_days),
                cutoff(age_anchor_ms, policy.gate_results_days),
                cutoff(age_anchor_ms, policy.terminal_merge_queue_days),
            )?
        } else {
            self.store().gc_row_candidates(
                cutoff(age_anchor_ms, policy.terminal_events_days),
                cutoff(age_anchor_ms, policy.gate_results_days),
                cutoff(age_anchor_ms, policy.terminal_merge_queue_days),
            )?
        };
        let checkpoint_pin_releases = if deadline_expired(deadline) {
            deferred_checks.push("checkpoint_pins".into());
            Vec::new()
        } else {
            self.store().gc_checkpoint_pin_candidates()?
        };
        let live_sessions = if deadline_expired(deadline) {
            deferred_checks.push("live_sessions".into());
            Vec::new()
        } else {
            self.store().live_sessions()?
        };
        let advisories = if deadline_expired(deadline) {
            deferred_checks.push("advisories".into());
            Vec::new()
        } else {
            self.store().advisories(false)?
        };
        let exposures = if deadline_expired(deadline) {
            deferred_checks.push("entry_path_exposures".into());
            Vec::new()
        } else {
            self.store().outstanding_entry_path_exposures()?
        };
        let operations = if deadline_expired(deadline) {
            deferred_checks.push("coordinated_operations".into());
            Vec::new()
        } else {
            self.store().coordinated_operations()?
        };
        let publication_exposure_expiries = exposures
            .iter()
            .filter(|exposure| exposure.created_at < exposure_cutoff)
            .map(|exposure| GcPublicationExposureExpiry {
                exposure_id: exposure.id,
                queue_entry_id: exposure.queue_entry_id,
                created_at: exposure.created_at,
                age_days: days_between(age_anchor_ms, exposure.created_at),
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
        let mut gate_log_inventory_deferred = false;
        for row in rows.drain(..) {
            if deadline_expired(deadline) {
                gate_log_inventory_deferred = true;
                retained_rows.push(row);
                continue;
            }
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
            if let Some(file) = files.get_mut(&relative) {
                file.source_row_ids.push(row.id);
                retained_rows.push(row);
                continue;
            }
            let (before_sha256, bytes_before) = if deadline.is_some() {
                match sha256_file_until(&path, deadline) {
                    Ok(Some((digest, bytes))) => (digest, bytes),
                    Ok(None) => {
                        gate_log_inventory_deferred = true;
                        retained_rows.push(row);
                        continue;
                    }
                    Err(error) => {
                        blockers.push(GcBlocker {
                            kind: "gate_log".into(),
                            id: Some(row.id),
                            reason: format!(
                                "cannot read gate log for bounded health scan: {error}"
                            ),
                        });
                        gate_log_inventory_deferred = true;
                        retained_rows.push(row);
                        continue;
                    }
                }
            } else {
                let bytes = std::fs::read(&path).map_err(|source| crate::BrokerError::Io {
                    path: path.clone(),
                    source,
                })?;
                (sha256(&bytes), bytes.len() as u64)
            };
            files.insert(
                relative.clone(),
                GcFileCandidate {
                    path: relative,
                    action: GcFileAction::Delete,
                    before_sha256,
                    after_sha256: None,
                    bytes_before,
                    bytes_after: 0,
                    source_row_ids: vec![row.id],
                },
            );
            retained_rows.push(row);
        }
        rows = retained_rows;
        if gate_log_inventory_deferred {
            deferred_checks.push("gate_log_inventory".into());
        }

        match metrics_rewrite(
            &main_root,
            cutoff(age_anchor_ms, policy.command_metrics_days),
            &mut blockers,
            deadline,
        )? {
            MetricsRewrite::Complete(Some(metrics)) => {
                files.insert(metrics.path.clone(), metrics);
            }
            MetricsRewrite::Complete(None) => {}
            MetricsRewrite::Deferred => deferred_checks.push("command_metrics".into()),
        }

        let worktree_cutoff = cutoff(age_anchor_ms, policy.closed_worktrees_days);
        let live_worktrees = live_sessions
            .iter()
            .map(|session| PathBuf::from(&session.worktree_path))
            .collect::<Vec<_>>();
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
                && policy.closed_worktree_in_grace(session, evaluated_at)
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
        let ArtifactCandidates {
            candidates: mut artifacts,
            declined: declined_artifacts,
            not_scanned: artifact_worktrees_not_scanned,
        } = match deadline {
            Some(deadline) => self.artifact_candidates_within(
                age_anchor_ms,
                &policy,
                &cleanup.worktrees,
                &sessions,
                ArtifactExclusions {
                    already_removed: &removed_sessions,
                    live_worktrees: &live_worktrees,
                },
                scan,
                None,
                Some(deadline),
            ),
            None => self.artifact_candidates(
                age_anchor_ms,
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
            self.orphan_candidates(age_anchor_ms, &policy, &mut blockers, scan, deadline)?;
        if !orphans_complete {
            deferred_checks.push("orphan_scan".into());
        }
        let (recovery_archives, recovery_archive_inventory, archives_complete) =
            self.recovery_archive_candidates(age_anchor_ms, &policy, scan, deadline)?;
        if !archives_complete {
            deferred_checks.push("recovery_archives".into());
        }
        // Rooted at the per-user cache directory, not at any worktree: that
        // is why no total here ever counted it (#295).
        let (mut size_records, size_records_complete) = match deadline {
            Some(deadline) => {
                match crate::measurement::load_size_records_until(&main_root, deadline) {
                    Some(records) => (records, true),
                    None => (crate::measurement::SizeRecords::default(), false),
                }
            }
            None => (crate::measurement::load_size_records(&main_root), true),
        };
        let (gate_cache, gate_caches) = if !size_records_complete || deadline_expired(deadline) {
            deferred_checks.push("gate_cache_inventory".into());
            (None, Vec::new())
        } else {
            // A configuration that does not load names no key, which leaves
            // every kind to the most-recent-use fallback.
            let configured_keys = crate::gates::load_gates(&main_root)
                .map(|gates| {
                    gates
                        .into_iter()
                        .filter_map(|gate| gate.managed_cache.map(|cache| cache.key))
                        .collect()
                })
                .unwrap_or_default();
            let gate_storage = self.storage_context().gate_storage_locations();
            match crate::gate_cache_gc::location_in(&main_root, &gate_storage) {
                Some(location) => {
                    if let Some(deadline) = deadline {
                        let (inventory, candidates, complete) = crate::gate_cache_gc::inspect_until(
                            &main_root,
                            &location,
                            crate::gate_cache_gc::KeepRule {
                                budget_bytes: policy.gate_cache_bytes_budget,
                                include_active: include_active_gate_cache,
                                configured_keys: &configured_keys,
                            },
                            crate::gate_cache_gc::AgeReference {
                                evaluated_at,
                                age_anchor_ms,
                            },
                            scan,
                            &mut size_records,
                            deadline,
                        );
                        if complete {
                            (inventory, candidates)
                        } else {
                            deferred_checks.push("gate_cache_inventory".into());
                            (None, Vec::new())
                        }
                    } else {
                        let (inventory, candidates) = crate::gate_cache_gc::inspect(
                            &main_root,
                            &location,
                            crate::gate_cache_gc::KeepRule {
                                budget_bytes: policy.gate_cache_bytes_budget,
                                include_active: include_active_gate_cache,
                                configured_keys: &configured_keys,
                            },
                            crate::gate_cache_gc::AgeReference {
                                evaluated_at,
                                age_anchor_ms,
                            },
                            scan,
                            &mut size_records,
                        );
                        (Some(inventory), candidates)
                    }
                }
                None => {
                    if deadline.is_some() {
                        deferred_checks.push("gate_cache_inventory".into());
                    }
                    (None, Vec::new())
                }
            }
        };
        if scan.measures() && size_records_complete {
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
                    oldest_recorded_at.map(|recorded_at| days_between(age_anchor_ms, recorded_at));
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
                    .saturating_sub(i64::from(artifact.idle_days).saturating_mul(MILLIS_PER_DAY)),
                estimated_bytes: artifact.estimated_bytes,
            }
        });
        // Orphaned roots carry no age -- their owning database is gone, which
        // is what makes them orphans -- so the budget rule has nothing extra
        // to say about them and routing them through it would be ordering
        // theatre. Bytes are the only fact available, and the largest
        // abandoned root is the one a bounded apply should reach first either
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
        if deadline_expired(deadline) {
            deferred_checks.push("plan_deadline".into());
            deferred_checks.sort();
            deferred_checks.dedup();
        }
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
            age_anchor_ms: Some(age_anchor_ms),
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
        if scan.measures() && plan.deferred_checks.is_empty() {
            plan.finish_digest()?;
        }
        if deadline_expired(deadline) {
            plan.digest.clear();
            plan.deferred_checks.push("plan_deadline".into());
            plan.deferred_checks.sort();
            plan.deferred_checks.dedup();
            if plan.budget_verdict == crate::BudgetVerdict::Within {
                plan.budget_verdict = crate::BudgetVerdict::Unknown;
            }
            plan.clears_retained_bytes_budget = false;
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
        self.gc_apply_with(confirm, None, true, None)
    }

    /// CLI apply uses the same scan mode as the plan command. An existing
    /// journal is still resumed directly; this deadline only covers the fresh
    /// plan needed when no journal exists.
    pub(crate) fn gc_apply_for_cli(
        &mut self,
        confirm: &str,
        include_active_gate_cache: bool,
        full_scan: bool,
    ) -> Result<GcApplyReport, BrokerOpError> {
        let planning_deadline = (!full_scan).then(|| Instant::now() + MANUAL_GC_PLAN_BUDGET);
        self.gc_apply_with(confirm, None, include_active_gate_cache, planning_deadline)
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
    /// Startup resumes only a durable GC plan that an operator already
    /// authorized. Its bounded autonomous sweep may reclaim rebuildable
    /// artifacts and remove a closed checkout only when the existing policy
    /// and ownership proofs pass; it does not create cleanup authority.
    pub(crate) fn resume_gc_maintenance(&mut self) -> Result<Option<GcApplyReport>, BrokerOpError> {
        let continuation_worker = std::env::var_os(ARTIFACT_SWEEP_WORKER_ENV).is_some();
        if continuation_worker {
            // This process must not resume an operator-authorized GC journal.
            // It enters only the explicit artifact or whole-checkout request
            // lane after Broker::open returns.
            return Ok(None);
        }
        // Rotation publishes a durable witness before it changes the shared
        // cache store, then requests the detached worker. A crash between
        // those operations must not leave the retired store undiscoverable
        // until another preparation writer runs. This bounded, read-only
        // probe uses the Broker-captured cache root and only requests the
        // existing cache-only continuation; it is never deletion authority.
        crate::warn_unrecorded(
            "record preparation-cache rotation recovery request",
            self.request_recovery_for_rotation_debt(),
        );
        let policy = load_retention_policy(self.main_root())?;
        let journal_path = self.main_root().join(".aethyme/gc-journal.json");
        let resumed = match load_journal(&journal_path)? {
            Some(journal) => {
                Some(self.apply_confirmed_gc_journal(journal, policy.startup_budget_ms)?)
            }
            None => None,
        };
        // The sweep only reclaims rebuildable caches, and this runs inside
        // broker open: its failure must not stop every command.
        crate::warn_unrecorded(
            "run bounded inline artifact sweep",
            self.sweep_artifacts_autonomously(&policy, ArtifactSweepScope::Inline),
        );
        // Inline admission stays capped at 250 ms. The detached worker
        // continues cache recovery from its own request and whole-checkout
        // cleanup only from its separate deferral request.
        crate::warn_unrecorded(
            "start artifact sweep continuation worker",
            self.maybe_start_artifact_sweep_worker(false),
        );
        Ok(resumed)
    }

    /// Resume a confirmed GC journal without probing preparation-cache debt,
    /// sweeping artifacts, or admitting the detached maintenance worker.
    /// Explicit cleanup resolve uses this open-time path because it must keep
    /// normal writable-open recovery while avoiding unrelated cleanup work.
    pub(crate) fn resume_confirmed_gc_journal_only(
        &mut self,
    ) -> Result<Option<GcApplyReport>, BrokerOpError> {
        if std::env::var_os(ARTIFACT_SWEEP_WORKER_ENV).is_some() {
            // Worker processes have their own scoped continuation lane and
            // must not resume operator-authorized whole-repository journals.
            return Ok(None);
        }
        let journal_path = self.main_root().join(".aethyme/gc-journal.json");
        let Some(journal) = load_journal(&journal_path)? else {
            return Ok(None);
        };
        let policy = load_retention_policy(self.main_root())?;
        self.apply_confirmed_gc_journal(journal, policy.startup_budget_ms)
            .map(Some)
    }

    fn apply_confirmed_gc_journal(
        &mut self,
        journal: GcJournal,
        startup_budget_ms: u64,
    ) -> Result<GcApplyReport, BrokerOpError> {
        self.gc_apply_bounded(&journal.digest, Some(startup_budget_ms))
    }

    fn request_recovery_for_rotation_debt(&mut self) -> Result<(), BrokerOpError> {
        let storage = self.storage_context();
        let cache_root_bindings = self.preparation_cache_root_bindings_for_maintenance()?;
        let probe_deadline = Instant::now() + PREPARATION_ROTATION_DEBT_PROBE_BUDGET;
        let probe = if let Some(resource_db_path) = storage.host_resource_db_path.as_deref() {
            if cache_root_bindings.is_empty() {
                crate::preparation::rotation_debt::DebtProbe::Unknown
            } else {
                let mut aggregate = crate::preparation::rotation_debt::DebtProbe::NoDebt;
                for binding in &cache_root_bindings {
                    let current = if binding.validate().is_ok() {
                        crate::preparation::probe_rotation_recovery(
                            &binding.path,
                            binding.identity.as_deref(),
                            resource_db_path,
                            PREPARATION_ROTATION_DEBT_PROBE_MAX_ENTRIES,
                            probe_deadline,
                        )
                    } else {
                        crate::preparation::rotation_debt::DebtProbe::Unknown
                    };
                    aggregate = match (aggregate, current) {
                        (_, crate::preparation::rotation_debt::DebtProbe::Unknown)
                        | (crate::preparation::rotation_debt::DebtProbe::Unknown, _) => {
                            crate::preparation::rotation_debt::DebtProbe::Unknown
                        }
                        (_, crate::preparation::rotation_debt::DebtProbe::Pending)
                        | (crate::preparation::rotation_debt::DebtProbe::Pending, _) => {
                            crate::preparation::rotation_debt::DebtProbe::Pending
                        }
                        _ => crate::preparation::rotation_debt::DebtProbe::NoDebt,
                    };
                    if aggregate == crate::preparation::rotation_debt::DebtProbe::Unknown {
                        break;
                    }
                }
                aggregate
            }
        } else {
            crate::preparation::rotation_debt::DebtProbe::Unknown
        };
        if probe == crate::preparation::rotation_debt::DebtProbe::NoDebt
            || self.artifact_sweep_recovery_requested()?
        {
            return Ok(());
        }
        // Keep one durable request pending while the root is Pending or
        // Unknown. Do not bump its generation on every ordinary open: a
        // previous worker may have intentionally set retry backoff after
        // making no progress.
        self.persist_artifact_sweep_recovery_request()
    }

    /// Run the autonomous artifact sweep now with the full policy budget,
    /// scaled by disk pressure, regardless of cadence. Safety rules are the
    /// ones broker open applies; only the budget and the schedule differ.
    pub fn gc_sweep_artifacts(&mut self) -> Result<ArtifactSweepReport, BrokerOpError> {
        let policy = load_retention_policy(self.main_root())?;
        let (budget_ms, urgency) = self.artifact_sweep_budget(&policy, ArtifactSweepScope::Drain);
        let outcome = self.sweep_artifacts_autonomously(&policy, ArtifactSweepScope::Drain)?;
        // The launcher admits artifact, checkout-cleanup and compaction work.
        // A compact-only backlog must not depend on a checkout-cleanup request.
        crate::warn_unrecorded(
            "start deferred artifact or checkout recovery worker",
            self.maybe_start_artifact_sweep_worker(false),
        );
        Ok(ArtifactSweepReport {
            directories_reclaimed: outcome.directories_reclaimed,
            complete: outcome.complete,
            budget_ms,
            disk_pressured: urgency == crate::disk_headroom::SweepUrgency::Pressured,
            legacy_unscoped_state: self.legacy_artifact_sweep_state(),
            auto_cleanup: outcome.auto_cleanup,
        })
    }

    /// Continue an unfinished artifact sweep with the full budget, if one is pending.
    /// `finish` pays the deferred artifact work while leaving the separate
    /// whole-checkout cleanup lane to the general sweep scopes.
    pub(crate) fn continue_artifact_sweep_backlog(
        &mut self,
    ) -> Result<Option<ArtifactSweepOutcome>, BrokerOpError> {
        if !self.artifact_sweep_backlog_pending()? {
            return Ok(None);
        }
        let policy = load_retention_policy(self.main_root())?;
        self.sweep_artifacts_autonomously(&policy, ArtifactSweepScope::FinishDrain)
            .map(Some)
    }

    /// Persist an automatic recovery request when an admission failure or an
    /// unfinished bounded sweep has exposed durable cache backlog. The marker
    /// is intentionally separate from the cadence/cursor state: normal broker
    /// opens can request a worker without pretending that the foreground pass
    /// finished or resetting its resume point.
    /// Record the durable request without launching the detached process.
    /// Preparation uses this while holding its shared-cache lease, then starts
    /// the worker only after the lease has been released successfully.
    pub(crate) fn persist_artifact_sweep_recovery_request(&mut self) -> Result<(), BrokerOpError> {
        let prior = self
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        // Treat every pressure refusal as a fresh generation. The worker
        // compares this token before clearing its request, so a new refusal
        // racing the end of a complete lap cannot disappear.
        let requested_at = now_ms().max(prior.saturating_add(1)).max(1);
        self.artifact_sweep_meta_set(
            ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY,
            &requested_at.to_string(),
        )?;
        self.artifact_sweep_meta_set(ARTIFACT_SWEEP_RETRY_AFTER_KEY, "")?;
        Ok(())
    }

    /// Persist a whole-checkout cleanup generation for an atomically queued
    /// terminal obligation. This is used after the close transaction and
    /// acquires the GC lock itself.
    pub(crate) fn request_checkout_cleanup_after_close(
        &mut self,
        session_id: i64,
    ) -> Result<(), BrokerOpError> {
        let root = self.main_root().to_path_buf();
        let _lock = GcLock::acquire(&root)?;
        self.request_checkout_cleanup_after_close_under_gc_lock(session_id)
    }

    /// Under-lock half of [`Self::request_checkout_cleanup_after_close`].
    /// Callers already holding `GcLock` must use this method to avoid
    /// recursive lock acquisition.
    pub(crate) fn request_checkout_cleanup_after_close_under_gc_lock(
        &mut self,
        session_id: i64,
    ) -> Result<(), BrokerOpError> {
        let Some(request) = self.store().pending_checkout_cleanup_request(session_id)? else {
            return Ok(());
        };
        if request.completed_at_ms.is_some() {
            return Ok(());
        }
        let session = self.store().session(session_id)?;
        if session.origin != crate::SessionOrigin::Spawned
            || !session.status.is_closed()
            || session.cleanup_state != crate::SessionCleanupState::Closed
        {
            return Ok(());
        }
        let cleanup_allowed =
            crate::AutoCleanupPolicy::load(self.main_root()).is_ok_and(|policy| {
                policy.auto_remove && !policy.is_pinned(Path::new(&session.worktree_path))
            });
        if !cleanup_allowed {
            return Ok(());
        }
        self.queue_pending_checkout_cleanup_generation(request.not_before_ms)
    }

    fn queue_pending_checkout_cleanup_generation(
        &mut self,
        not_before_ms: i64,
    ) -> Result<(), BrokerOpError> {
        let requested = self
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = self
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let generation = if requested > completed {
            requested
        } else {
            now_ms()
                .max(requested.saturating_add(1))
                .max(completed.saturating_add(1))
                .max(1)
        };
        if generation != requested {
            self.artifact_sweep_meta_set(
                AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY,
                &generation.to_string(),
            )?;
        }
        let next_due = self
            .store()
            .next_pending_checkout_cleanup_at()?
            .unwrap_or(not_before_ms)
            .min(not_before_ms);
        // Routine outbox reconciliation must not clear a retained target's
        // cooldown merely because its original grace period has elapsed.
        let now = now_ms();
        let existing_retry = self
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|retry| *retry > now)
            .unwrap_or(0);
        let retry_after = if next_due > now {
            next_due.max(existing_retry)
        } else {
            existing_retry
        };
        self.artifact_sweep_meta_set(
            AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY,
            &if retry_after > now {
                retry_after.to_string()
            } else {
                String::new()
            },
        )?;
        Ok(())
    }

    /// Recover a crash between the session-close transaction and its worker
    /// wake-up. The outbox is small and indexed by due time; it does not scan
    /// session history.
    fn reconcile_checkout_cleanup_outbox(&mut self) -> Result<(), BrokerOpError> {
        let Some(next_due) = self.store().next_pending_checkout_cleanup_at()? else {
            return Ok(());
        };
        let root = self.main_root().to_path_buf();
        let Ok(_lock) = GcLock::acquire(&root) else {
            return Ok(());
        };
        if !crate::AutoCleanupPolicy::load(self.main_root()).is_ok_and(|policy| policy.auto_remove)
        {
            return Ok(());
        }
        self.queue_pending_checkout_cleanup_generation(next_due)
    }

    fn is_session_retirement_state_unavailable(error: &BrokerOpError) -> bool {
        matches!(
            error,
            BrokerOpError::Store(
                crate::BrokerError::SessionRetirementDatabaseBusy
                    | crate::BrokerError::InvalidSessionRetirementState(_)
            )
        )
    }

    fn note_invalid_session_retirement_state(&mut self, error: &BrokerOpError) {
        if matches!(
            error,
            BrokerOpError::Store(crate::BrokerError::InvalidSessionRetirementState(_))
        ) {
            match self
                .store()
                .meta_get_for_session_retirement(ARTIFACT_SWEEP_WORKER_STATUS_KEY)
            {
                Ok(current) if current.as_deref() == Some("session_retirement_state_invalid") => {}
                Ok(current) => crate::warn_unrecorded(
                    "record invalid session-retirement state",
                    self.store()
                        .meta_compare_and_set_for_session_retirement(
                            ARTIFACT_SWEEP_WORKER_STATUS_KEY,
                            current.as_deref(),
                            "session_retirement_state_invalid",
                        )
                        .map(|_| ()),
                ),
                Err(crate::BrokerError::SessionRetirementDatabaseBusy) => {}
                Err(error) => crate::warn_unrecorded(
                    "record invalid session-retirement state",
                    Err::<(), _>(error),
                ),
            }
        }
    }

    fn note_session_retirement_state_unavailable(&mut self, error: &BrokerOpError) {
        match error {
            BrokerOpError::Store(crate::BrokerError::SessionRetirementDatabaseBusy) => {
                crate::warn_unrecorded(
                    "read or update durable session-retirement state",
                    Err::<(), _>(crate::BrokerError::SessionRetirementDatabaseBusy),
                );
            }
            BrokerOpError::Store(crate::BrokerError::InvalidSessionRetirementState(_)) => {
                self.note_invalid_session_retirement_state(error);
                // Keep the diagnostic stable and avoid echoing persisted bytes.
                crate::warn_unrecorded(
                    "read durable session-retirement state",
                    Err::<(), _>("invalid versioned state"),
                );
            }
            _ => {}
        }
    }

    fn load_session_retirement_state(
        &mut self,
    ) -> Result<(Option<String>, SessionRetirementRequestState), BrokerOpError> {
        let raw = self
            .store()
            .meta_get_for_session_retirement(SESSION_RETIREMENT_STATE_KEY)?;
        let Some(encoded) = raw.as_deref() else {
            return Ok((raw, SessionRetirementRequestState::default()));
        };
        let state =
            serde_json::from_str::<SessionRetirementRequestState>(encoded).map_err(|error| {
                crate::BrokerError::InvalidSessionRetirementState(format!(
                    "malformed versioned state: {error}"
                ))
            })?;
        if state.version != SESSION_RETIREMENT_STATE_VERSION
            || state.requested_generation < 0
            || state.completed_generation < 0
            || state.completed_generation > state.requested_generation
            || state.retry_after_ms < 0
        {
            return Err(crate::BrokerError::InvalidSessionRetirementState(
                "unsupported or inconsistent versioned state".into(),
            )
            .into());
        }
        Ok((raw, state))
    }

    fn store_session_retirement_state(
        &mut self,
        expected: Option<&str>,
        state: &SessionRetirementRequestState,
    ) -> Result<bool, BrokerOpError> {
        let encoded = serde_json::to_string(state).map_err(|error| {
            crate::BrokerError::InvalidSessionRetirementState(format!(
                "could not encode versioned state: {error}"
            ))
        })?;
        self.store()
            .meta_compare_and_set_for_session_retirement(
                SESSION_RETIREMENT_STATE_KEY,
                expected,
                &encoded,
            )
            .map_err(Into::into)
    }

    fn next_session_retirement_generation(
        state: &SessionRetirementRequestState,
    ) -> Result<i64, BrokerOpError> {
        let requested = state.requested_generation.checked_add(1).ok_or_else(|| {
            crate::BrokerError::InvalidSessionRetirementState("request generation overflow".into())
        })?;
        let completed = state.completed_generation.checked_add(1).ok_or_else(|| {
            crate::BrokerError::InvalidSessionRetirementState(
                "completion generation overflow".into(),
            )
        })?;
        Ok(now_ms().max(requested).max(completed).max(1))
    }

    fn session_retirement_request_state(&mut self) -> Result<(i64, i64), BrokerOpError> {
        let (_, state) = self.load_session_retirement_state()?;
        Ok((state.requested_generation, state.completed_generation))
    }

    fn session_retirement_recovery_state(&mut self) -> Result<Option<bool>, BrokerOpError> {
        match self.session_retirement_request_state() {
            Ok((requested, completed)) => Ok(Some(requested > completed)),
            Err(error) if Self::is_session_retirement_state_unavailable(&error) => {
                self.note_session_retirement_state_unavailable(&error);
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn session_retirement_recovery_requested(&mut self) -> Result<bool, BrokerOpError> {
        Ok(self.session_retirement_recovery_state()?.unwrap_or(false))
    }

    fn session_retirement_due_generation(
        &mut self,
        now: i64,
    ) -> Result<Option<i64>, BrokerOpError> {
        match self.load_session_retirement_state() {
            Ok((_, state))
                if state.requested_generation > state.completed_generation
                    && state.retry_after_ms <= now =>
            {
                Ok(Some(state.requested_generation))
            }
            Ok(_) => Ok(None),
            Err(error) if Self::is_session_retirement_state_unavailable(&error) => {
                self.note_session_retirement_state_unavailable(&error);
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn request_session_retirement_if_due(
        &mut self,
        policy: &RetentionPolicy,
        scope: ArtifactSweepScope,
    ) -> Result<Option<i64>, BrokerOpError> {
        if !scope_runs_checkout_cleanup(scope) || policy.session_abandoned_after_hours == 0 {
            return Ok(None);
        }
        for _ in 0..SESSION_RETIREMENT_STATE_CAS_ATTEMPTS {
            let (raw, mut state) = match self.load_session_retirement_state() {
                Ok(value) => value,
                Err(error) if Self::is_session_retirement_state_unavailable(&error) => {
                    self.note_session_retirement_state_unavailable(&error);
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            if state.requested_generation > state.completed_generation {
                return Ok(Some(state.requested_generation));
            }
            if state.retry_after_ms > now_ms() {
                return Ok(None);
            }
            let generation = Self::next_session_retirement_generation(&state)?;
            state.requested_generation = generation;
            state.retry_after_ms = 0;
            match self.store_session_retirement_state(raw.as_deref(), &state) {
                Ok(true) => return Ok(Some(generation)),
                Ok(false) => {}
                Err(error) if Self::is_session_retirement_state_unavailable(&error) => {
                    self.note_session_retirement_state_unavailable(&error);
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
        }
        crate::warn_unrecorded(
            "enqueue durable session-retirement request",
            Err::<(), _>("compare-and-set contention"),
        );
        Ok(None)
    }

    fn persist_session_retirement_request(
        &mut self,
        report: &crate::AutoCleanupReport,
    ) -> Result<(), BrokerOpError> {
        if report.retirement_deferred_reasons.is_empty() {
            return Ok(());
        }
        for _ in 0..SESSION_RETIREMENT_STATE_CAS_ATTEMPTS {
            let (raw, mut state) = match self.load_session_retirement_state() {
                Ok(value) => value,
                Err(error) if Self::is_session_retirement_state_unavailable(&error) => {
                    self.note_session_retirement_state_unavailable(&error);
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            if state.requested_generation > state.completed_generation
                || state.retry_after_ms > now_ms()
            {
                return Ok(());
            }
            state.requested_generation = Self::next_session_retirement_generation(&state)?;
            state.retry_after_ms = 0;
            match self.store_session_retirement_state(raw.as_deref(), &state) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) if Self::is_session_retirement_state_unavailable(&error) => {
                    self.note_session_retirement_state_unavailable(&error);
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        crate::warn_unrecorded(
            "persist deferred session-retirement request",
            Err::<(), _>("compare-and-set contention"),
        );
        Ok(())
    }

    /// Complete only the generation whose retirement pass still owns GcLock.
    /// The request, completion and retry deadline share one CAS state row, so
    /// completion can advance only while the captured request is still current.
    fn complete_session_retirement_request(
        &mut self,
        request_generation: i64,
    ) -> Result<(), BrokerOpError> {
        for _ in 0..SESSION_RETIREMENT_STATE_CAS_ATTEMPTS {
            let (raw, mut state) = self.load_session_retirement_state()?;
            if state.requested_generation != request_generation
                || state.requested_generation <= state.completed_generation
            {
                return Ok(());
            }
            state.completed_generation = request_generation;
            state.retry_after_ms = now_ms().saturating_add(SESSION_RETIREMENT_WORKER_RETRY_MS);
            if self.store_session_retirement_state(raw.as_deref(), &state)? {
                return Ok(());
            }
        }
        Err(crate::BrokerError::SessionRetirementDatabaseBusy.into())
    }

    fn defer_session_retirement_request(
        &mut self,
        request_generation: i64,
        reason: &str,
    ) -> Result<(), BrokerOpError> {
        for _ in 0..SESSION_RETIREMENT_STATE_CAS_ATTEMPTS {
            let (raw, mut state) = self.load_session_retirement_state()?;
            if state.requested_generation != request_generation
                || state.requested_generation <= state.completed_generation
            {
                return Ok(());
            }
            state.retry_after_ms = now_ms().saturating_add(SESSION_RETIREMENT_WORKER_RETRY_MS);
            if self.store_session_retirement_state(raw.as_deref(), &state)? {
                return self.write_artifact_sweep_worker_status(reason);
            }
        }
        Err(crate::BrokerError::SessionRetirementDatabaseBusy.into())
    }

    /// Record a separate continuation request only when a general automatic
    /// checkout-cleanup pass deferred work. This is called under the sweep's
    /// GC lock; cache backlog alone and FinishDrain never create cleanup
    /// authority.
    fn persist_auto_checkout_cleanup_request(
        &mut self,
        scope: ArtifactSweepScope,
        report: &crate::auto_cleanup::AutoCleanupReport,
    ) -> Result<(), BrokerOpError> {
        if !matches!(
            scope,
            ArtifactSweepScope::Inline | ArtifactSweepScope::Drain
        ) {
            return Ok(());
        }
        let retirement_deferred = !report.retirement_deferred_reasons.is_empty();
        self.persist_session_retirement_request(report)?;
        let checkout_deferred = report.deferred > report.retirement_deferred_reasons.len();
        if (!report.enabled && !retirement_deferred) || !checkout_deferred {
            return Ok(());
        }
        let requested = self
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = self
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        // Keep an existing incomplete generation idempotent. The worker
        // completes it while holding the same lock as this ordinary pass, so
        // a later deferral necessarily creates a fresh generation.
        if requested > completed {
            return Ok(());
        }
        let generation = now_ms()
            .max(requested.saturating_add(1))
            .max(completed.saturating_add(1))
            .max(1);
        self.artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY, &generation.to_string())?;
        self.artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY, "")?;
        Ok(())
    }

    /// Whether a durable whole-checkout cleanup request remains incomplete.
    fn auto_checkout_cleanup_recovery_requested(&mut self) -> Result<bool, BrokerOpError> {
        let requested = self
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = self
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let automatic_removal_enabled =
            crate::AutoCleanupPolicy::load(self.main_root()).is_ok_and(|policy| policy.auto_remove);
        Ok(requested > completed
            || (automatic_removal_enabled
                && self.store().next_pending_checkout_cleanup_at()?.is_some()))
    }

    fn auto_checkout_cleanup_retry_pending(&mut self, now: i64) -> Result<bool, BrokerOpError> {
        let metadata_retry = self
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .is_some_and(|retry_after| retry_after > now);
        let outbox_retry = self
            .store()
            .next_pending_checkout_cleanup_at()?
            .is_some_and(|retry_after| retry_after > now);
        Ok(metadata_retry || outbox_retry)
    }

    /// Detect the opt-in edge and queue a durable one-time stock backfill.
    /// This is metadata-only on broker open; path enumeration stays in the
    /// detached worker.
    fn reconcile_compact_checkout_backfill(&mut self) -> Result<bool, BrokerOpError> {
        let policy = load_retention_policy(self.main_root())?;
        let enabled_marker = self.artifact_sweep_meta_get(AUTO_COMPACT_POLICY_ENABLED_KEY)?;
        if !policy.compact_closed_worktrees {
            if enabled_marker.as_deref() != Some("0") {
                self.artifact_sweep_meta_set(AUTO_COMPACT_POLICY_ENABLED_KEY, "0")?;
            }
            return Ok(false);
        }

        let requested = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_COMPLETED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        if enabled_marker.as_deref() != Some("1") && requested <= completed {
            let generation = now_ms()
                .max(requested.saturating_add(1))
                .max(completed.saturating_add(1))
                .max(1);
            // Reset the cursor first. If the process stops before the request
            // write, the next opt-in observation repeats these harmless
            // resets; a durable generation can never inherit a completed
            // cursor from the prior opt-in.
            self.artifact_sweep_meta_set(AUTO_COMPACT_BACKFILL_CURSOR_KEY, "0")?;
            self.artifact_sweep_meta_set(AUTO_COMPACT_BACKFILL_HIGH_WATER_KEY, "")?;
            self.artifact_sweep_meta_set(AUTO_COMPACT_BACKFILL_RETRY_AFTER_KEY, "")?;
            self.artifact_sweep_meta_set(
                AUTO_COMPACT_BACKFILL_REQUESTED_KEY,
                &generation.to_string(),
            )?;
        }
        if enabled_marker.as_deref() != Some("1") {
            self.artifact_sweep_meta_set(AUTO_COMPACT_POLICY_ENABLED_KEY, "1")?;
        }
        let requested = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_COMPLETED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        Ok(requested > completed)
    }

    fn compact_checkout_backfill_pending(&mut self) -> Result<bool, BrokerOpError> {
        let requested = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_COMPLETED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        Ok(requested > completed)
    }

    fn compact_checkout_work_pending(&mut self) -> Result<bool, BrokerOpError> {
        let policy = load_retention_policy(self.main_root())?;
        if !policy.compact_closed_worktrees {
            return Ok(false);
        }
        Ok(self.compact_checkout_backfill_pending()?
            || self
                .store()
                .next_pending_checkout_compaction_at()?
                .is_some())
    }

    fn compact_checkout_work_due(&mut self, now: i64) -> Result<bool, BrokerOpError> {
        let policy = load_retention_policy(self.main_root())?;
        if !policy.compact_closed_worktrees {
            return Ok(false);
        }
        let retry_after = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_RETRY_AFTER_KEY)?
            .and_then(|value| value.parse::<i64>().ok());
        let next_outbox_due = self.store().next_pending_checkout_compaction_at()?;
        Ok(compact_checkout_pending_is_due(
            self.compact_checkout_backfill_pending()?,
            retry_after,
            next_outbox_due,
            now,
        ))
    }

    /// Apply bounded backoff to both the stock seed and currently due rows.
    /// The request rows remain durable and are still subject to fresh proof.
    fn defer_auto_compact_checkout_worker(
        &mut self,
        reason: &str,
        retry_ms: i64,
    ) -> Result<(), BrokerOpError> {
        let now = now_ms();
        let retry_after = now.saturating_add(retry_ms);
        if self.compact_checkout_backfill_pending()? {
            self.artifact_sweep_meta_set(
                AUTO_COMPACT_BACKFILL_RETRY_AFTER_KEY,
                &retry_after.to_string(),
            )?;
        }
        // Store limits each batch to 128 rows. Repeating a bounded number of
        // times also cools down the current stock burst without walking
        // session history on an ordinary broker open.
        for _ in 0..8 {
            let due = self.store().due_checkout_cleanup_compactions(now, 128)?;
            if due.is_empty() {
                break;
            }
            self.store()
                .defer_checkout_cleanup_compactions(&due, retry_after, now)?;
        }
        self.write_artifact_sweep_worker_status(reason)
    }

    /// Reconcile the crash window after a verified checkout removal and the
    /// terminal session-row update. This never removes paths: only an exact
    /// terminal row whose recorded path is now absent can have its outbox
    /// obligations acknowledged.
    fn reconcile_terminal_checkout_cleanup_requests(
        &mut self,
        now: i64,
    ) -> Result<usize, BrokerOpError> {
        let terminal_ids = self.store().pending_terminal_checkout_cleanup_ids(
            now,
            0,
            AUTO_COMPACT_BACKFILL_PAGE_SIZE,
        )?;
        let mut completed = 0usize;
        for session_id in terminal_ids {
            let session = match self.store().session(session_id) {
                Ok(session)
                    if session.status == crate::SessionStatus::Cleaned
                        && session.cleanup_state == crate::SessionCleanupState::Cleaned =>
                {
                    session
                }
                _ => {
                    self.store()
                        .defer_terminal_checkout_cleanup_reconciliation(&[session_id], now)?;
                    continue;
                }
            };
            let path = Path::new(&session.worktree_path);
            match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    completed = completed.saturating_add(
                        self.store()
                            .complete_checkout_cleanup_requests_after_removal(&[session_id], now)?,
                    );
                }
                Ok(_) | Err(_) => {
                    self.store()
                        .defer_terminal_checkout_cleanup_reconciliation(&[session_id], now)?;
                }
            }
        }
        Ok(completed)
    }

    /// Seed witnessed legacy Spawned rows from the complete bounded path
    /// index. The Store rechecks origin, terminal state, cleanup state and
    /// ownership witness transactionally before it creates any request.
    fn seed_compact_checkout_backfill_pagewise(
        &mut self,
        request_generation: i64,
        high_water_session_id: i64,
        deadline: Instant,
        grace_hours: u32,
    ) -> Result<(bool, usize), BrokerOpError> {
        let requested = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_COMPLETED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        if requested != request_generation || completed >= request_generation {
            return Ok((false, 0));
        }

        let stored_high_water = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_HIGH_WATER_KEY)?
            .and_then(|value| value.parse::<i64>().ok());
        let high_water = stored_high_water.unwrap_or(high_water_session_id);
        if stored_high_water.is_none() {
            self.artifact_sweep_meta_set(
                AUTO_COMPACT_BACKFILL_HIGH_WATER_KEY,
                &high_water.to_string(),
            )?;
        }
        let mut after = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_CURSOR_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let mut scheduled = 0usize;
        while after < high_water && Instant::now() < deadline {
            let rows = self.store().cleanup_cohort_indexed_rows_page(
                crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
                after,
                AUTO_COMPACT_BACKFILL_PAGE_SIZE,
            )?;
            let rows = rows
                .into_iter()
                .take_while(|row| row.session_id <= high_water)
                .collect::<Vec<_>>();
            let Some(last) = rows.last().map(|row| row.session_id) else {
                after = high_water;
                self.artifact_sweep_meta_set(AUTO_COMPACT_BACKFILL_CURSOR_KEY, &after.to_string())?;
                break;
            };
            let closed_ids = rows
                .iter()
                .filter(|row| row.cleanup_state == "closed")
                .map(|row| row.session_id)
                .collect::<Vec<_>>();
            if !closed_ids.is_empty() {
                scheduled = scheduled.saturating_add(
                    self.store().request_closed_spawned_checkout_compactions(
                        &closed_ids,
                        grace_hours,
                        now_ms(),
                    )?,
                );
            }
            after = last;
            self.artifact_sweep_meta_set(AUTO_COMPACT_BACKFILL_CURSOR_KEY, &after.to_string())?;
        }

        if after >= high_water {
            let current_requested = self
                .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_REQUESTED_KEY)?
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(0);
            let current_completed = self
                .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_COMPLETED_KEY)?
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(0);
            if current_requested == request_generation
                && current_completed < request_generation
                && load_retention_policy(self.main_root())?.compact_closed_worktrees
            {
                self.artifact_sweep_meta_set(
                    AUTO_COMPACT_BACKFILL_COMPLETED_KEY,
                    &request_generation.to_string(),
                )?;
                self.artifact_sweep_meta_set(AUTO_COMPACT_BACKFILL_RETRY_AFTER_KEY, "")?;
                return Ok((true, scheduled));
            }
        }
        Ok((false, scheduled))
    }

    /// Run one detached compact-maintenance pass. The alias index gets one
    /// resumable full refresh; the indexed inventory and candidate cursor are
    /// then consumed within the same reserved pass. Each attempt rebuilds its
    /// cohort and keeps the full archive reserve; the batch and worker lifetime
    /// remain bounded independently.
    fn auto_compact_checkout_recovery_pass(
        &mut self,
        deadline: Instant,
    ) -> Result<(), BrokerOpError> {
        let retention = load_retention_policy(self.main_root())?;
        if !retention.compact_closed_worktrees {
            return Ok(());
        }
        if deadline.saturating_duration_since(Instant::now())
            < crate::auto_cleanup::AUTO_COMPACTION_MIN_BUDGET
        {
            return self.defer_auto_compact_checkout_worker(
                "compact_checkout_reserve_unavailable",
                AUTO_COMPACT_BACKFILL_RETRY_MS,
            );
        }

        let request_generation = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed_generation = self
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_COMPLETED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let backfill_pending = request_generation > completed_generation;
        let root = self.main_root().to_path_buf();
        let Ok(gc_lock) = GcLock::acquire(&root) else {
            return self.defer_auto_compact_checkout_worker(
                "compact_checkout_gc_lock_busy",
                AUTO_COMPACT_BACKFILL_RETRY_MS,
            );
        };

        // Leave the helper's minimum archive/removal reserve untouched while
        // the full alias lap and bounded stock pages run.
        let refresh_deadline = deadline
            .checked_sub(crate::auto_cleanup::AUTO_COMPACTION_MIN_BUDGET)
            .unwrap_or(deadline);
        let mut refreshed_index = None;
        let mut last_progress_sequence = None;
        loop {
            if Instant::now() >= refresh_deadline {
                break;
            }
            let Some(state) = self.refresh_cleanup_cohort_aliases(Some(refresh_deadline))? else {
                break;
            };
            if state.complete {
                refreshed_index = Some(state);
                break;
            }
            if last_progress_sequence == Some(state.progress_sequence) {
                break;
            }
            last_progress_sequence = Some(state.progress_sequence);
        }
        let Some(index_state) = refreshed_index
            .filter(|state| state.unresolved_rows == 0 && state.pending_dirty_rows == 0)
        else {
            drop(gc_lock);
            return self.defer_auto_compact_checkout_worker(
                "compact_checkout_alias_index_incomplete",
                AUTO_COMPACT_BACKFILL_RETRY_MS,
            );
        };

        let mut scheduled = 0usize;
        let backfill_complete = if backfill_pending {
            let (complete, count) = self.seed_compact_checkout_backfill_pagewise(
                request_generation,
                index_state.high_water_session_id,
                refresh_deadline,
                retention.closed_worktree_grace_hours,
            )?;
            scheduled = scheduled.saturating_add(count);
            complete
        } else {
            true
        };
        drop(gc_lock);

        if !backfill_complete {
            self.artifact_sweep_meta_set(
                AUTO_COMPACT_BACKFILL_RETRY_AFTER_KEY,
                &now_ms()
                    .saturating_add(AUTO_COMPACT_BACKFILL_RETRY_MS)
                    .to_string(),
            )?;
        }
        if deadline.saturating_duration_since(Instant::now())
            < crate::auto_cleanup::AUTO_COMPACTION_MIN_BUDGET
        {
            return self.defer_auto_compact_checkout_worker(
                "compact_checkout_worker_budget_exhausted",
                AUTO_COMPACT_BACKFILL_RETRY_MS,
            );
        }

        let due = self
            .store()
            .due_checkout_cleanup_compactions(now_ms(), AUTO_COMPACT_CHECKOUT_BATCH_SIZE)?;
        if due.is_empty() {
            let status = if backfill_complete {
                format!("compact_checkout_seeded={scheduled} no_due_candidates")
            } else {
                format!("compact_checkout_seed_progress={scheduled}")
            };
            self.write_artifact_sweep_worker_status(&status)?;
            return Ok(());
        }

        let complete = run_compact_checkout_candidates(
            &due,
            || deadline.saturating_duration_since(Instant::now()),
            |session_id| match self.auto_compact_checkout_candidate(*session_id, deadline) {
                Ok(()) => Ok(()),
                Err(error @ BrokerOpError::Store(_)) => Err(error),
                Err(error) => {
                    self.store().defer_checkout_cleanup_compactions(
                        &[*session_id],
                        now_ms().saturating_add(AUTO_COMPACT_CHECKOUT_RETRY_MS),
                        now_ms(),
                    )?;
                    self.write_artifact_sweep_worker_status(&format!(
                        "compact_checkout_candidate_failed session={session_id}: {error}"
                    ))
                }
            },
        )?;
        if !complete {
            self.defer_auto_compact_checkout_worker(
                "compact_checkout_batch_budget_exhausted",
                AUTO_COMPACT_BACKFILL_RETRY_MS,
            )?;
        }
        Ok(())
    }

    /// Rebuild and verify ownership for each candidate; never reuse a prior
    /// checkout's cohort, liveness result, archive proof or lifecycle claim.
    fn auto_compact_checkout_candidate(
        &mut self,
        session_id: i64,
        deadline: Instant,
    ) -> Result<(), BrokerOpError> {
        let session = self.store().session(session_id)?;
        if session.origin != crate::SessionOrigin::Spawned
            || !session.status.is_closed()
            || session.cleanup_state != crate::SessionCleanupState::Closed
        {
            self.store().defer_checkout_cleanup_compactions(
                &[session_id],
                now_ms().saturating_add(AUTO_COMPACT_CHECKOUT_RETRY_MS),
                now_ms(),
            )?;
            return self.write_artifact_sweep_worker_status("compact_checkout_candidate_changed");
        }
        let worktree = PathBuf::from(&session.worktree_path);
        let cohort = match self.closed_artifact_checkout_cohort_for_session_detailed(
            session_id,
            &worktree,
            Some(deadline),
        )? {
            crate::auto_cleanup::ClosedArtifactCheckoutCohortResolution::Available(cohort) => {
                cohort
            }
            crate::auto_cleanup::ClosedArtifactCheckoutCohortResolution::Unavailable(reason) => {
                self.store().defer_checkout_cleanup_compactions(
                    &[session_id],
                    now_ms().saturating_add(AUTO_COMPACT_CHECKOUT_RETRY_MS),
                    now_ms(),
                )?;
                return self.write_artifact_sweep_worker_status(&format!(
                    "compact_checkout_cohort_unavailable session={session_id} reason={}",
                    reason.code()
                ));
            }
        };
        if cohort.owner_session_id() != session_id || cohort.owner_witness().is_none() {
            self.store().defer_checkout_cleanup_compactions(
                &[session_id],
                now_ms().saturating_add(AUTO_COMPACT_CHECKOUT_RETRY_MS),
                now_ms(),
            )?;
            return self.write_artifact_sweep_worker_status("compact_checkout_owner_unverified");
        }
        let cleanup_policy = match crate::AutoCleanupPolicy::load(self.main_root()) {
            Ok(policy) => policy,
            Err(reason) => {
                self.store().defer_checkout_cleanup_compactions(
                    &[session_id],
                    now_ms().saturating_add(AUTO_COMPACT_CHECKOUT_RETRY_MS),
                    now_ms(),
                )?;
                return self.write_artifact_sweep_worker_status(&format!(
                    "compact_checkout_cleanup_policy_unavailable: {reason}"
                ));
            }
        };
        if !cleanup_policy.auto_remove || cleanup_policy.is_pinned(&worktree) {
            self.store().defer_checkout_cleanup_compactions(
                &[session_id],
                now_ms().saturating_add(AUTO_COMPACT_CHECKOUT_RETRY_MS),
                now_ms(),
            )?;
            return self.write_artifact_sweep_worker_status(if cleanup_policy.auto_remove {
                "compact_checkout_pinned_by_cleanup_policy"
            } else {
                "compact_checkout_auto_remove_disabled"
            });
        }

        match self.compact_closed_checkout(&cohort, deadline)? {
            Some(outcome) if outcome.worktree_removed => {
                self.store()
                    .complete_checkout_cleanup_requests_after_removal(
                        &cohort.session_ids(),
                        now_ms(),
                    )?;
                self.write_artifact_sweep_worker_status(
                    "compact_checkout_removed; next due rows remain wakeable",
                )?;
            }
            Some(_) | None => {
                self.store().defer_checkout_cleanup_compactions(
                    &cohort.session_ids(),
                    now_ms().saturating_add(AUTO_COMPACT_CHECKOUT_RETRY_MS),
                    now_ms(),
                )?;
                self.write_artifact_sweep_worker_status(
                    "compact_checkout_deferred; request retained",
                )?;
            }
        }
        Ok(())
    }

    /// Launch recovery after the request is durable and any conflicting
    /// preparation lease has been released.
    pub(crate) fn start_artifact_sweep_recovery_worker(&mut self) -> Result<(), BrokerOpError> {
        self.maybe_start_artifact_sweep_worker(true)?;
        Ok(())
    }

    /// Build the continuation command without changing which database it opens.
    /// The private worker rejects test database overrides, so omit the pin only
    /// after the child's real no-override resolver proves the same database.
    pub(crate) fn artifact_sweep_worker_command(
        &mut self,
        router: &Path,
        canonical_root: &Path,
        gate_scope: Option<&std::ffi::OsStr>,
    ) -> Result<Command, BrokerOpError> {
        let child_database = crate::gate_database::resolve(canonical_root, None, gate_scope)
            .map_err(|reason| BrokerOpError::InvalidStorageContext {
                reason: reason.into(),
            })?;
        if self.store().db_path() != child_database {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: "automatic recovery cannot omit a non-default broker database binding"
                    .into(),
            });
        }
        let mut command = Command::new(router);
        command
            .args(["broker", "gc", "__continue-artifacts"])
            .arg(canonical_root)
            .current_dir(canonical_root)
            .env(ARTIFACT_SWEEP_WORKER_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.configure_storage_context_for_child(&mut command)?;
        command.env_remove(crate::BROKER_DB_ENV);
        // Pass the same scope snapshot used by the equality proof.
        match gate_scope {
            Some(scope) => command.env(crate::gate_database::SCOPE_ENV, scope),
            None => command.env_remove(crate::gate_database::SCOPE_ENV),
        };
        Ok(command)
    }

    /// Launch the installed router as a detached, request-scoped continuation.
    /// The OS advisory lock is the singleton; its file stays content-free.
    fn maybe_start_artifact_sweep_worker(
        &mut self,
        pressure_request: bool,
    ) -> Result<(), BrokerOpError> {
        #[cfg(test)]
        tests::ARTIFACT_WORKER_ADMISSION_ATTEMPTS
            .with(|attempts| attempts.borrow_mut().push(pressure_request));
        self.reconcile_checkout_cleanup_outbox()?;
        let root = self.main_root().to_path_buf();
        if crate::host_state::path_is_ephemeral(&root) || artifact_worker_process_is_suppressed() {
            return Ok(());
        }
        self.reconcile_compact_checkout_backfill()?;
        let artifact_pending = self.artifact_sweep_backlog_pending()?
            && crate::AutoCleanupPolicy::artifact_sweep_allowed(&root);
        let checkout_cleanup_pending = self.auto_checkout_cleanup_recovery_requested()?;
        let session_retirement_pending = self.session_retirement_recovery_requested()?;
        let compact_pending = self.compact_checkout_work_pending()?;
        if !(artifact_pending
            || checkout_cleanup_pending
            || session_retirement_pending
            || compact_pending)
        {
            return Ok(());
        }
        let now = now_ms();
        let mut artifact_due = artifact_pending;
        let checkout_cleanup_due =
            checkout_cleanup_pending && !self.auto_checkout_cleanup_retry_pending(now)?;
        let session_retirement_due =
            session_retirement_pending && self.session_retirement_due_generation(now)?.is_some();
        let compact_due = compact_pending && self.compact_checkout_work_due(now)?;
        if !pressure_request {
            let namespace = self.artifact_sweep_namespace()?.to_string();
            artifact_due &= !worker_retry_pending(self.store(), &namespace, now)?;
        }
        if !(artifact_due || checkout_cleanup_due || session_retirement_due || compact_due) {
            return Ok(());
        }
        let Some((router, _engine)) = artifact_worker_binaries() else {
            if artifact_due {
                self.defer_artifact_sweep_worker("runtime_unverified")?;
            }
            if checkout_cleanup_due {
                self.defer_auto_checkout_cleanup_worker("runtime_unverified")?;
            }
            if session_retirement_due {
                self.defer_session_retirement_worker("session_retirement_runtime_unverified")?;
            }
            if compact_due {
                self.defer_auto_compact_checkout_worker(
                    "compact_checkout_runtime_unverified",
                    AUTO_COMPACT_BACKFILL_RETRY_MS,
                )?;
            }
            return Ok(());
        };
        let Some(lock) = try_artifact_sweep_worker_lock(&root)? else {
            return Ok(());
        };
        if !artifact_worker_runtime_is_aligned() {
            if artifact_due {
                self.defer_artifact_sweep_worker("runtime_pair_unverified")?;
            }
            if checkout_cleanup_due {
                self.defer_auto_checkout_cleanup_worker("runtime_pair_unverified")?;
            }
            if session_retirement_due {
                self.defer_session_retirement_worker("session_retirement_runtime_pair_unverified")?;
            }
            if compact_due {
                self.defer_auto_compact_checkout_worker(
                    "compact_checkout_runtime_pair_unverified",
                    AUTO_COMPACT_BACKFILL_RETRY_MS,
                )?;
            }
            return Ok(());
        }
        let canonical_root =
            std::fs::canonicalize(&root).map_err(|source| crate::BrokerError::Io {
                path: root.clone(),
                source,
            })?;
        let gate_scope = std::env::var_os(crate::gate_database::SCOPE_ENV);
        let mut command = match self.artifact_sweep_worker_command(
            &router,
            &canonical_root,
            gate_scope.as_deref(),
        ) {
            Ok(command) => command,
            Err(_) => {
                if artifact_due {
                    self.defer_artifact_sweep_worker("storage_context_unavailable")?;
                }
                if checkout_cleanup_due {
                    self.defer_auto_checkout_cleanup_worker("storage_context_unavailable")?;
                }
                if session_retirement_due {
                    self.defer_session_retirement_worker(
                        "session_retirement_storage_context_unavailable",
                    )?;
                }
                if compact_due {
                    self.defer_auto_compact_checkout_worker(
                        "compact_checkout_storage_context_unavailable",
                        AUTO_COMPACT_BACKFILL_RETRY_MS,
                    )?;
                }
                return Ok(());
            }
        };
        configure_artifact_worker_process_group(&mut command);
        let _child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                if artifact_due {
                    self.defer_artifact_sweep_worker(&format!("worker_launch_failed: {error}"))?;
                }
                if checkout_cleanup_due {
                    self.defer_auto_checkout_cleanup_worker(&format!(
                        "worker_launch_failed: {error}"
                    ))?;
                }
                if session_retirement_due {
                    self.defer_session_retirement_worker(&format!(
                        "session_retirement_worker_launch_failed: {error}"
                    ))?;
                }
                if compact_due {
                    self.defer_auto_compact_checkout_worker(
                        &format!("compact_checkout_worker_launch_failed: {error}"),
                        AUTO_COMPACT_BACKFILL_RETRY_MS,
                    )?;
                }
                return Ok(());
            }
        };
        // The advisory lock is intentionally content-free. Writing a PID
        // here could truncate another file if the lock path were hard-linked
        // between validation and open. Dropping the parent lock lets the
        // child (which already exists and is blocked on the same OS lock) take
        // over without a PID race.
        drop(lock);
        Ok(())
    }

    /// Execute the detached worker's bounded artifact and requested cleanup
    /// lanes. The artifact loop itself remains cache-only. Each lane retains
    /// its own request when a pass is incomplete and backs off after repeated
    /// no-progress passes so open files or concurrent writers cannot keep the
    /// background process alive forever. Checkout candidate discovery currently
    /// scans the full cohort before checking the pass deadline, so the reserved
    /// tranche is cooperative and can overrun on a large cohort; the durable
    /// cursor bounds proof progress after discovery, not discovery time.
    pub(crate) fn run_artifact_sweep_recovery_worker(&mut self) -> Result<(), BrokerOpError> {
        if !matches!(std::env::var(ARTIFACT_SWEEP_WORKER_ENV).as_deref(), Ok("1")) {
            self.write_artifact_sweep_worker_status("refused_environment")?;
            return Ok(());
        }
        if crate::host_state::path_is_ephemeral(self.main_root()) {
            self.write_artifact_sweep_worker_status("refused_ephemeral_repository")?;
            return Ok(());
        }
        self.reconcile_checkout_cleanup_outbox()?;
        self.reconcile_compact_checkout_backfill()?;
        let reconciled_terminal_rows =
            self.reconcile_terminal_checkout_cleanup_requests(now_ms())?;
        if reconciled_terminal_rows > 0 {
            self.write_artifact_sweep_worker_status(&format!(
                "terminal_checkout_requests_reconciled={reconciled_terminal_rows}"
            ))?;
        }
        let artifact_pending = self.artifact_sweep_backlog_pending()?
            && crate::AutoCleanupPolicy::artifact_sweep_allowed(self.main_root());
        let checkout_cleanup_pending = self.auto_checkout_cleanup_recovery_requested()?;
        let session_retirement_state = self.session_retirement_recovery_state()?;
        let session_retirement_pending = session_retirement_state.unwrap_or(false);
        let compact_pending = self.compact_checkout_work_pending()?;
        if !(artifact_pending
            || checkout_cleanup_pending
            || session_retirement_pending
            || compact_pending)
        {
            if session_retirement_state.is_some() {
                self.write_artifact_sweep_worker_status("no_backlog")?;
            }
            return Ok(());
        }
        let namespace = self.artifact_sweep_namespace()?.to_string();
        let now = now_ms();
        let artifact_due =
            artifact_pending && !worker_retry_pending(self.store(), &namespace, now)?;
        let checkout_cleanup_due =
            checkout_cleanup_pending && !self.auto_checkout_cleanup_retry_pending(now)?;
        let session_retirement_generation = if session_retirement_pending {
            self.session_retirement_due_generation(now)?
        } else {
            None
        };
        let session_retirement_due = session_retirement_generation.is_some();
        let compact_due = compact_pending && self.compact_checkout_work_due(now)?;
        if !(artifact_due || checkout_cleanup_due || session_retirement_due || compact_due) {
            self.write_artifact_sweep_worker_status("retry_deferred")?;
            return Ok(());
        }
        if !artifact_worker_runtime_is_aligned() {
            if artifact_due {
                self.defer_artifact_sweep_worker("runtime_pair_unverified")?;
            }
            if checkout_cleanup_due {
                self.defer_auto_checkout_cleanup_worker("runtime_pair_unverified")?;
            }
            if session_retirement_due {
                self.defer_session_retirement_worker("session_retirement_runtime_pair_unverified")?;
            }
            if compact_due {
                self.defer_auto_compact_checkout_worker(
                    "compact_checkout_runtime_pair_unverified",
                    AUTO_COMPACT_BACKFILL_RETRY_MS,
                )?;
            }
            return Ok(());
        }
        let worker_started = Instant::now();
        if let Some(request_generation) = session_retirement_generation {
            let remaining =
                ARTIFACT_SWEEP_WORKER_MAX_DURATION.saturating_sub(worker_started.elapsed());
            let pass_budget = remaining.min(SESSION_RETIREMENT_WORKER_PASS_BUDGET);
            self.run_session_retirement_recovery_worker_loop_isolated(
                request_generation,
                Some(SESSION_RETIREMENT_WORKER_MAX_PASSES),
                Some(pass_budget),
            )?;
        }
        let cache_lane_result = if artifact_due {
            (|| {
                let policy = load_retention_policy(self.main_root())?;
                if policy.artifact_sweep_budget_ms == 0 {
                    return self.defer_artifact_sweep_worker("artifact_sweep_disabled");
                }
                let request_generation = self
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)?
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(0);
                // Cache discovery/removal shares the singleton process with a
                // separate cleanup lane. It yields a reserved tranche whenever
                // any due lifecycle lane needs time, so a wide cache backlog
                // cannot starve retirement or checkout progress.
                let cache_duration = artifact_worker_cache_lane_duration(
                    checkout_cleanup_due,
                    compact_due,
                    session_retirement_due,
                );
                self.run_artifact_sweep_recovery_worker_loop(
                    &policy,
                    request_generation,
                    Some(ARTIFACT_SWEEP_WORKER_MAX_PASSES),
                    Some(cache_duration),
                    |broker, policy, remaining| {
                        let deadline = remaining.map(|remaining| Instant::now() + remaining);
                        broker.sweep_artifacts_autonomously_with_deadline(
                            policy,
                            ArtifactSweepScope::AutomaticContinuation,
                            deadline,
                        )
                    },
                )
            })()
        } else {
            Ok(())
        };

        self.run_cleanup_after_cache_lane(cache_lane_result, |broker| {
            // Compact archives run only in this detached worker. A fresh
            // wake may have arrived during the cache lane, so recheck policy,
            // the backfill marker and per-session due times here.
            let compact_pending = broker.compact_checkout_work_pending()?;
            let compact_due = compact_pending && broker.compact_checkout_work_due(now_ms())?;
            let cleanup_pending_before_compact = if compact_due {
                broker.auto_checkout_cleanup_recovery_requested()?
            } else {
                false
            };
            let cleanup_due_before_compact = cleanup_pending_before_compact
                && !broker.auto_checkout_cleanup_retry_pending(now_ms())?;
            if compact_due {
                let remaining =
                    ARTIFACT_SWEEP_WORKER_MAX_DURATION.saturating_sub(worker_started.elapsed());
                let remaining_for_compact =
                    remaining.saturating_sub(SESSION_RETIREMENT_WORKER_RESERVE);
                if let Some(pass_budget) = compact_checkout_worker_pass_budget(
                    remaining_for_compact,
                    cleanup_due_before_compact,
                ) {
                    let deadline = Instant::now() + pass_budget;
                    if let Err(error) = broker.auto_compact_checkout_recovery_pass(deadline) {
                        if matches!(error, BrokerOpError::Store(_)) {
                            return Err(error);
                        }
                        broker.defer_auto_compact_checkout_worker(
                            &format!("compact_checkout_pass_failed: {error}"),
                            AUTO_COMPACT_CHECKOUT_RETRY_MS,
                        )?;
                    }
                } else {
                    broker.defer_auto_compact_checkout_worker(
                        "compact_checkout_worker_reserve_unavailable",
                        AUTO_COMPACT_BACKFILL_RETRY_MS,
                    )?;
                }
            }

            // A cleanup request may have arrived while the cache lane was
            // working. Re-read its generation instead of letting the worker
            // exit with newly queued work stranded behind its singleton lock.
            let cleanup_pending = broker.auto_checkout_cleanup_recovery_requested()?;
            let cleanup_due =
                cleanup_pending && !broker.auto_checkout_cleanup_retry_pending(now_ms())?;
            if cleanup_due {
                let request_generation = broker
                    .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)?
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(0);
                let remaining =
                    ARTIFACT_SWEEP_WORKER_MAX_DURATION.saturating_sub(worker_started.elapsed());
                broker.run_auto_checkout_cleanup_recovery_worker_loop(
                    request_generation,
                    Some(AUTO_CHECKOUT_CLEANUP_WORKER_MAX_PASSES),
                    Some(remaining.saturating_sub(SESSION_RETIREMENT_WORKER_RESERVE)),
                    |broker, generation, pass_budget| {
                        let Some(pass_budget) = pass_budget else {
                            return Ok(None);
                        };
                        let deadline = Instant::now() + pass_budget;
                        broker.auto_checkout_cleanup_recovery_pass(generation, deadline)
                    },
                )?;
            }

            // A request may arrive while this process owns the singleton
            // worker lock and is spending its cache, compaction, or checkout
            // tranche. Re-read it last and use only the reserved time left
            // under the original five-minute worker cap.
            if let Some(request_generation) = broker.session_retirement_due_generation(now_ms())? {
                let remaining =
                    ARTIFACT_SWEEP_WORKER_MAX_DURATION.saturating_sub(worker_started.elapsed());
                if remaining.is_zero() {
                    broker.defer_session_retirement_request(
                        request_generation,
                        "session_retirement_post_cache_reserve_unavailable",
                    )?;
                    broker.write_artifact_sweep_worker_status(
                        "session_retirement_post_cache_reserve_unavailable",
                    )?;
                } else {
                    broker.run_session_retirement_recovery_worker_loop_isolated(
                        request_generation,
                        Some(SESSION_RETIREMENT_WORKER_MAX_PASSES),
                        Some(remaining.min(SESSION_RETIREMENT_WORKER_PASS_BUDGET)),
                    )?;
                }
            }
            Ok(())
        })?;
        Ok(())
    }

    fn run_cleanup_after_cache_lane(
        &mut self,
        cache_lane_result: Result<(), BrokerOpError>,
        cleanup_lane: impl FnOnce(&mut Self) -> Result<(), BrokerOpError>,
    ) -> Result<(), BrokerOpError> {
        match cache_lane_result {
            Ok(()) => {}
            Err(error) if matches!(error, BrokerOpError::Store(_)) => return Err(error),
            Err(error) => {
                self.defer_artifact_sweep_worker(&format!("cache_lane_failed: {error}"))?
            }
        }
        cleanup_lane(self)
    }

    fn session_retirement_recovery_pass(
        &mut self,
        request_generation: i64,
        deadline: Instant,
    ) -> Result<Option<(crate::AutoCleanupReport, GcLock)>, BrokerOpError> {
        let root = self.main_root().to_path_buf();
        let Ok(_lock) = GcLock::acquire(&root) else {
            return Ok(None);
        };
        let (requested, completed) = self.session_retirement_request_state()?;
        if requested != request_generation || completed >= request_generation {
            return Ok(None);
        }
        self.retire_abandoned_sessions_for_recovery(Some(deadline))
            .map(|report| Some((report, _lock)))
    }

    fn run_session_retirement_recovery_worker_loop(
        &mut self,
        request_generation: i64,
        max_passes: Option<usize>,
        max_duration: Option<Duration>,
    ) -> Result<(), BrokerOpError> {
        let started_at = Instant::now();
        let mut passes = 0usize;
        let mut aggregate = crate::AutoCleanupReport::default();
        let mut has_unresolved_retirement_reasons = false;
        let mut continuation_reason = None;
        loop {
            let remaining = max_duration.map(|limit| limit.saturating_sub(started_at.elapsed()));
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                if let Some(reason) = continuation_reason.take() {
                    aggregate.deferred = aggregate.deferred.saturating_add(1);
                    aggregate.retirement_deferred_reasons.push(reason);
                }
                self.record_auto_cleanup_report(&aggregate)?;
                self.defer_session_retirement_request(
                    request_generation,
                    &format!("session_retirement_time_limit passes={passes}"),
                )?;
                return Ok(());
            }
            if max_passes.is_some_and(|limit| passes >= limit) {
                if let Some(reason) = continuation_reason.take() {
                    aggregate.deferred = aggregate.deferred.saturating_add(1);
                    aggregate.retirement_deferred_reasons.push(reason);
                }
                self.record_auto_cleanup_report(&aggregate)?;
                self.defer_session_retirement_request(
                    request_generation,
                    &format!("session_retirement_pass_limit passes={passes}"),
                )?;
                return Ok(());
            }
            passes = passes.saturating_add(1);
            let pass_budget = remaining
                .map(|remaining| remaining.min(SESSION_RETIREMENT_WORKER_PASS_BUDGET))
                .unwrap_or(SESSION_RETIREMENT_WORKER_PASS_BUDGET);
            let deadline = Instant::now() + pass_budget;
            let (report, pass_lock) =
                match self.session_retirement_recovery_pass(request_generation, deadline)? {
                    Some(pass) => pass,
                    None => {
                        let (requested, completed) = self.session_retirement_request_state()?;
                        if requested == request_generation && completed < request_generation {
                            if let Some(reason) = continuation_reason.take() {
                                aggregate.deferred = aggregate.deferred.saturating_add(1);
                                aggregate.retirement_deferred_reasons.push(reason);
                            }
                            self.record_auto_cleanup_report(&aggregate)?;
                            self.defer_session_retirement_request(
                                request_generation,
                                "session_retirement_lock_busy",
                            )?;
                        }
                        return Ok(());
                    }
                };
            aggregate.enabled = report.enabled;
            aggregate.config_error = report.config_error.clone();
            aggregate.evaluated_at_ms = report.evaluated_at_ms;
            aggregate.sessions_retired = aggregate
                .sessions_retired
                .saturating_add(report.sessions_retired);
            let pagination_reasons = report
                .retirement_deferred_reasons
                .iter()
                .filter(|reason| {
                    matches!(
                        reason,
                        SessionRetirementDeferredReason::MoreSessionsRemain { .. }
                    )
                })
                .count();
            aggregate.deferred = aggregate
                .deferred
                .saturating_add(report.deferred.saturating_sub(pagination_reasons));
            let has_more_sessions = report.retirement_deferred_reasons.iter().any(|reason| {
                matches!(
                    reason,
                    SessionRetirementDeferredReason::MoreSessionsRemain { .. }
                )
            });
            continuation_reason = report
                .retirement_deferred_reasons
                .iter()
                .find(|reason| {
                    matches!(
                        reason,
                        SessionRetirementDeferredReason::MoreSessionsRemain { .. }
                    )
                })
                .cloned();
            has_unresolved_retirement_reasons |=
                report.retirement_deferred_reasons.iter().any(|reason| {
                    !matches!(
                        reason,
                        SessionRetirementDeferredReason::MoreSessionsRemain { .. }
                    )
                });
            aggregate.retirement_deferred_reasons.extend(
                report
                    .retirement_deferred_reasons
                    .into_iter()
                    .filter(|reason| {
                        !matches!(
                            reason,
                            SessionRetirementDeferredReason::MoreSessionsRemain { .. }
                        )
                    }),
            );
            if has_more_sessions {
                drop(pass_lock);
                continue;
            }
            self.record_auto_cleanup_report(&aggregate)?;
            if !has_unresolved_retirement_reasons {
                self.complete_session_retirement_request(request_generation)?;
                self.write_artifact_sweep_worker_status(&format!(
                    "session_retirement_complete passes={passes} retired={}",
                    aggregate.sessions_retired
                ))?;
            } else {
                self.defer_session_retirement_request(
                    request_generation,
                    &format!("session_retirement_deferred passes={passes}"),
                )?;
            }
            drop(pass_lock);
            return Ok(());
        }
    }

    fn run_session_retirement_recovery_worker_loop_isolated(
        &mut self,
        request_generation: i64,
        max_passes: Option<usize>,
        max_duration: Option<Duration>,
    ) -> Result<(), BrokerOpError> {
        match self.run_session_retirement_recovery_worker_loop(
            request_generation,
            max_passes,
            max_duration,
        ) {
            Ok(()) => Ok(()),
            Err(error) if Self::is_session_retirement_state_unavailable(&error) => {
                self.note_session_retirement_state_unavailable(&error);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// The request and completed generation are read and updated while holding
    /// the same GC lock as the existing auto-cleanup plan/apply operation.
    /// No operator journal or cache request can enter this path.
    fn auto_checkout_cleanup_recovery_pass(
        &mut self,
        request_generation: i64,
        deadline: Instant,
    ) -> Result<Option<(crate::auto_cleanup::AutoCleanupReport, bool)>, BrokerOpError> {
        let root = self.main_root().to_path_buf();
        let Ok(_lock) = GcLock::acquire(&root) else {
            return Ok(None);
        };
        let requested = self
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = self
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        if completed >= request_generation {
            return Ok(Some((
                crate::auto_cleanup::AutoCleanupReport::default(),
                true,
            )));
        }
        if requested != request_generation {
            // This pass was scheduled for a stale generation. Leave the
            // newer request and its completion marker untouched.
            return Ok(Some((
                crate::auto_cleanup::AutoCleanupReport::default(),
                false,
            )));
        }
        let report = self.auto_remove_disposable_checkouts(Some(deadline))?;
        let removed_ids = report
            .removed
            .iter()
            .flat_map(|checkout| checkout.sessions.iter().copied())
            .collect::<Vec<_>>();
        if !removed_ids.is_empty() {
            self.store()
                .complete_checkout_cleanup_requests_after_removal(&removed_ids, now_ms())?;
        }
        let policy_valid = report.config_error.is_none();
        let no_deferred_work = report.deferred == 0;
        let mut generation_completed = false;
        if policy_valid && no_deferred_work {
            let latest_request = self
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)?
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(0);
            if latest_request == request_generation {
                self.artifact_sweep_meta_set(
                    AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY,
                    &request_generation.to_string(),
                )?;
                let now = now_ms();
                let retained_due = report.enabled
                    && self
                        .store()
                        .next_pending_checkout_cleanup_at()?
                        .is_some_and(|due| due <= now);
                let retry_after = if retained_due {
                    now.saturating_add(ARTIFACT_SWEEP_WORKER_RETRY_MS)
                        .to_string()
                } else {
                    String::new()
                };
                self.artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY, &retry_after)?;
                // Completing a scan generation grants no removal authority.
                // Retained per-session obligations remain pending and cooled.
                generation_completed = true;
            }
        }
        Ok(Some((report, generation_completed)))
    }

    fn run_auto_checkout_cleanup_recovery_worker_loop(
        &mut self,
        request_generation: i64,
        max_passes: Option<usize>,
        max_duration: Option<Duration>,
        mut cleanup_pass: impl FnMut(
            &mut Self,
            i64,
            Option<Duration>,
        ) -> Result<
            Option<(crate::auto_cleanup::AutoCleanupReport, bool)>,
            BrokerOpError,
        >,
    ) -> Result<(), BrokerOpError> {
        let started_at = Instant::now();
        let mut passes = 0usize;
        let mut no_progress_passes = 0usize;
        let mut removed_checkouts = 0usize;
        let mut seen_progress_markers = BTreeSet::new();
        loop {
            let remaining = max_duration.map(|limit| limit.saturating_sub(started_at.elapsed()));
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                self.defer_auto_checkout_cleanup_worker(&format!(
                    "checkout_cleanup_time_limit passes={passes} removed={removed_checkouts}"
                ))?;
                return Ok(());
            }
            passes = passes.saturating_add(1);
            let pass_budget = remaining
                .map(|remaining| remaining.min(AUTO_CHECKOUT_CLEANUP_WORKER_PASS_BUDGET))
                .or(Some(AUTO_CHECKOUT_CLEANUP_WORKER_PASS_BUDGET));
            let progress_before = self.auto_cleanup_progress_marker()?;
            seen_progress_markers.insert(progress_before);
            let pass = match cleanup_pass(self, request_generation, pass_budget) {
                Ok(pass) => pass,
                Err(error) => {
                    self.defer_auto_checkout_cleanup_worker(&format!(
                        "checkout_cleanup_failed: {error}"
                    ))?;
                    return Ok(());
                }
            };
            let progress_after = self.auto_cleanup_progress_marker()?;
            let discovery_progressed =
                progress_after != progress_before && seen_progress_markers.insert(progress_after);
            let Some((report, generation_completed)) = pass else {
                no_progress_passes = no_progress_passes.saturating_add(1);
                if no_progress_passes >= ARTIFACT_SWEEP_WORKER_NO_PROGRESS_PASSES {
                    self.defer_auto_checkout_cleanup_worker(&format!(
                        "checkout_cleanup_lock_busy passes={passes}"
                    ))?;
                    return Ok(());
                }
                let sleep = Duration::from_millis(1_000);
                let remaining = max_duration
                    .map(|limit| limit.saturating_sub(started_at.elapsed()))
                    .unwrap_or(sleep);
                std::thread::sleep(sleep.min(remaining));
                continue;
            };
            removed_checkouts = removed_checkouts.saturating_add(report.removed.len());
            if max_duration.is_some_and(|limit| started_at.elapsed() >= limit) {
                self.defer_auto_checkout_cleanup_worker(&format!(
                    "checkout_cleanup_time_limit passes={passes} removed={removed_checkouts}"
                ))?;
                return Ok(());
            }
            if generation_completed {
                self.write_artifact_sweep_worker_status(&format!(
                    "checkout_cleanup_complete passes={passes} removed={removed_checkouts}"
                ))?;
                return Ok(());
            }
            if max_passes.is_some_and(|limit| passes >= limit) {
                self.defer_auto_checkout_cleanup_worker(&format!(
                    "checkout_cleanup_pass_limit passes={passes} removed={removed_checkouts}"
                ))?;
                return Ok(());
            }
            if report.removed.is_empty() && !discovery_progressed {
                no_progress_passes = no_progress_passes.saturating_add(1);
                if no_progress_passes >= ARTIFACT_SWEEP_WORKER_NO_PROGRESS_PASSES {
                    self.defer_auto_checkout_cleanup_worker(&format!(
                        "checkout_cleanup_no_progress passes={passes} removed={removed_checkouts}"
                    ))?;
                    return Ok(());
                }
                let sleep = Duration::from_millis(1_000);
                let remaining = max_duration
                    .map(|limit| limit.saturating_sub(started_at.elapsed()))
                    .unwrap_or(sleep);
                std::thread::sleep(sleep.min(remaining));
            } else {
                no_progress_passes = 0;
            }
        }
    }

    fn defer_auto_checkout_cleanup_worker(&mut self, reason: &str) -> Result<(), BrokerOpError> {
        let retry_after = now_ms().saturating_add(ARTIFACT_SWEEP_WORKER_RETRY_MS);
        self.artifact_sweep_meta_set(
            AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY,
            &retry_after.to_string(),
        )?;
        self.write_artifact_sweep_worker_status(reason)
    }

    fn defer_session_retirement_worker(&mut self, reason: &str) -> Result<(), BrokerOpError> {
        let (requested, completed) = match self.session_retirement_request_state() {
            Ok(state) => state,
            Err(error) if Self::is_session_retirement_state_unavailable(&error) => {
                self.note_session_retirement_state_unavailable(&error);
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if requested > completed {
            self.defer_session_retirement_request(requested, reason)?;
        }
        Ok(())
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
        max_duration: Option<Duration>,
        mut sweep_pass: impl FnMut(
            &mut Self,
            &RetentionPolicy,
            Option<Duration>,
        ) -> Result<(ArtifactSweepOutcome, usize), BrokerOpError>,
    ) -> Result<(), BrokerOpError> {
        let started_at = Instant::now();
        let mut passes = 0usize;
        let mut no_progress_passes = 0usize;
        let mut entries_reclaimed = 0usize;
        loop {
            let remaining = max_duration.map(|limit| limit.saturating_sub(started_at.elapsed()));
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                self.defer_artifact_sweep_worker(&format!(
                    "time_limit passes={passes} entries={entries_reclaimed}"
                ))?;
                return Ok(());
            }
            passes = passes.saturating_add(1);
            let (outcome, pass_entries_reclaimed) = match sweep_pass(self, policy, remaining) {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.defer_artifact_sweep_worker(&format!("sweep_failed: {error}"))?;
                    return Ok(());
                }
            };
            entries_reclaimed = entries_reclaimed.saturating_add(pass_entries_reclaimed);
            if max_duration.is_some_and(|limit| started_at.elapsed() >= limit) {
                self.defer_artifact_sweep_worker(&format!(
                    "time_limit passes={passes} entries={entries_reclaimed}"
                ))?;
                return Ok(());
            }
            if outcome.complete {
                // Preserve request history and advance only the generation
                // this pass actually covered. A pressure refusal arriving
                // between this write and the next open remains pending.
                self.artifact_sweep_meta_set(
                    ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY,
                    &request_generation.to_string(),
                )?;
                self.artifact_sweep_meta_set(ARTIFACT_SWEEP_RETRY_AFTER_KEY, "")?;
                self.write_artifact_sweep_worker_status(&format!(
                    "complete passes={passes} entries={entries_reclaimed}"
                ))?;
                return Ok(());
            }
            if max_passes.is_some_and(|limit| passes >= limit) {
                // Keep the request and cursor pending for a later normal open
                // after bounded positive work. A test can set a smaller limit
                // to exercise process restart at this same checkpoint.
                self.defer_artifact_sweep_worker(&format!(
                    "pass_limit passes={passes} entries={entries_reclaimed}"
                ))?;
                return Ok(());
            }
            if pass_entries_reclaimed == 0 {
                no_progress_passes = no_progress_passes.saturating_add(1);
                if no_progress_passes >= ARTIFACT_SWEEP_WORKER_NO_PROGRESS_PASSES {
                    self.defer_artifact_sweep_worker(&format!(
                        "no_progress passes={passes} entries={entries_reclaimed}"
                    ))?;
                    return Ok(());
                }
                let sleep = Duration::from_millis(1_000);
                let remaining = max_duration
                    .map(|limit| limit.saturating_sub(started_at.elapsed()))
                    .unwrap_or(sleep);
                std::thread::sleep(sleep.min(remaining));
            } else {
                no_progress_passes = 0;
            }
        }
    }

    fn defer_artifact_sweep_worker(&mut self, reason: &str) -> Result<(), BrokerOpError> {
        let retry_after = now_ms().saturating_add(ARTIFACT_SWEEP_WORKER_RETRY_MS);
        self.artifact_sweep_meta_set(ARTIFACT_SWEEP_RETRY_AFTER_KEY, &retry_after.to_string())?;
        self.write_artifact_sweep_worker_status(reason)
    }

    fn write_artifact_sweep_worker_status(&mut self, status: &str) -> Result<(), BrokerOpError> {
        // The value is intentionally bounded and replaced rather than logged
        // to a file that could itself grow without limit.
        let bounded = status.chars().take(160).collect::<String>();
        self.artifact_sweep_meta_set(ARTIFACT_SWEEP_WORKER_STATUS_KEY, &bounded)?;
        Ok(())
    }

    /// Whether a recovery request remains durable. The worker advances a
    /// separate completed generation only after a complete cache-only lap;
    /// interruptions retain this marker and the existing sweep cursor/tree.
    pub(crate) fn artifact_sweep_recovery_requested(&mut self) -> Result<bool, BrokerOpError> {
        let requested = self
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = self
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        Ok(requested > completed)
    }

    /// An earlier pass stopped at its budget and has not finished a lap since.
    fn artifact_sweep_backlog_pending(&mut self) -> Result<bool, BrokerOpError> {
        let cursor = self
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_CURSOR_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let partial = self
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_PROGRESS_KEY)?
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
            .artifact_sweep_meta_get(key)?
            .and_then(|value| value.parse::<i64>().ok())
            .is_some_and(|last| last > 0 && now.saturating_sub(last) < window_ms))
    }

    fn artifact_sweep_budget(
        &mut self,
        policy: &RetentionPolicy,
        scope: ArtifactSweepScope,
    ) -> (u64, crate::disk_headroom::SweepUrgency) {
        let main_root = self.main_root().to_path_buf();
        let roots = self.artifact_sweep_budget_roots(scope);
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
                crate::disk_headroom::available_headroom_at_or_above_for(&main_root, &path);
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

    fn artifact_sweep_budget_roots(&mut self, scope: ArtifactSweepScope) -> BTreeSet<PathBuf> {
        let main_root = self.main_root().to_path_buf();
        // The detached worker can reclaim caches from external worktree and
        // preparation-cache volumes. Looking only at the main checkout misses
        // pressure on those filesystems (the main checkout is often elsewhere).
        let mut roots = BTreeSet::from([main_root.clone()]);
        if scope == ArtifactSweepScope::AutomaticContinuation {
            if let Ok(paths) = self.store().session_worktree_paths() {
                roots.extend(paths.into_iter().map(PathBuf::from));
            }
            if let Ok(cache_roots) = self.preparation_cache_root_bindings_for_maintenance() {
                roots.extend(cache_roots.into_iter().map(|binding| binding.path));
            }
        }
        roots
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
    /// confirmation. Every autonomous scope is closed-session-only: an open
    /// session can have a build process that outlives its agent, and shallow
    /// directory timestamps cannot prove that a deeply nested cache is idle.
    /// The reviewed `gc reclaim` lane remains available for open-session
    /// caches.
    ///
    /// Deliberately avoids [`Broker::cleanup_plan`]: sizing every retained
    /// worktree is a full stat walk over tens of gigabytes, far too expensive
    /// for a path that runs on every broker open. Discovery here is a bounded
    /// `read_dir` scan and sizes are never computed; the operator-invoked
    /// `gc plan` remains the surface that reports bytes.
    ///
    /// The budget bounds the work, not the number of directories attempted.
    /// A single `target/` can hold several gigabytes and take minutes to
    /// unlink, so removal stops at the deadline and records each in-progress
    /// path before unlinking. Later passes resume those exact paths and rotate
    /// the session cursor past incomplete worktrees so a wide or blocked
    /// checkout cannot starve the rest of the backlog (#222).
    #[expect(
        clippy::too_many_arguments,
        reason = "Keep cleanup evidence, queue state and output accumulators explicit at this private boundary"
    )]
    fn reclaim_ignored_directory_roots_under_claims(
        &mut self,
        root: &Path,
        expected_cohort: Option<&crate::auto_cleanup::ClosedArtifactCheckoutCohort>,
        candidates: &BTreeSet<PathBuf>,
        namespace: &str,
        queued_paths: &mut Vec<String>,
        queue_valid: &mut bool,
        cleanup_policy: &crate::AutoCleanupPolicy,
        deadline: Instant,
        blocked: &mut Vec<BlockedInterruptedPath>,
        removed: &mut Vec<String>,
    ) -> Result<(usize, bool), BrokerOpError> {
        if candidates.is_empty() {
            return Ok((0, true));
        }
        let Some(expected_cohort) = expected_cohort else {
            for path in candidates {
                blocked.push(BlockedInterruptedPath {
                    path: Some(path.to_string_lossy().into_owned()),
                    reason: "closed_checkout_cohort_unavailable",
                });
            }
            return Ok((0, false));
        };
        let owner_session_id = expected_cohort.owner_session_id();
        let _lifecycle_claim = match self.try_claim_worktree_lifecycle() {
            Ok(Some(claim)) => claim,
            Ok(None) => {
                for path in candidates {
                    blocked.push(BlockedInterruptedPath {
                        path: Some(path.to_string_lossy().into_owned()),
                        reason: "worktree_lifecycle_contended",
                    });
                }
                return Ok((0, false));
            }
            Err(_) => {
                for path in candidates {
                    blocked.push(BlockedInterruptedPath {
                        path: Some(path.to_string_lossy().into_owned()),
                        reason: "worktree_lifecycle_unavailable",
                    });
                }
                return Ok((0, false));
            }
        };
        if check_deadline(Some(deadline)) {
            return Ok((0, false));
        }
        if crate::AutoCleanupPolicy::load(self.main_root())
            .ok()
            .as_ref()
            != Some(cleanup_policy)
            || !ignored_directory_sweep_policy_allows_checkout(cleanup_policy, root)
        {
            for path in candidates {
                blocked.push(BlockedInterruptedPath {
                    path: Some(path.to_string_lossy().into_owned()),
                    reason: "ignored_directory_policy_changed",
                });
            }
            return Ok((0, false));
        }

        let artifact_use_reclaimer =
            match crate::file_lock::artifact_use::try_artifact_use_reclaimer(self.main_root(), root)
            {
                Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(reclaimer)) => {
                    reclaimer
                }
                Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(block)) => {
                    let reason = match block {
                        crate::file_lock::artifact_use::ArtifactUseReclaimBlock::Contended => {
                            "artifact_use_contended"
                        }
                        crate::file_lock::artifact_use::ArtifactUseReclaimBlock::UnknownProducer {
                            ..
                        } => "artifact_use_unknown_producer",
                        crate::file_lock::artifact_use::ArtifactUseReclaimBlock::ProtocolUnavailable => {
                            "artifact_use_protocol_unavailable"
                        }
                    };
                    for path in candidates {
                        blocked.push(BlockedInterruptedPath {
                            path: Some(path.to_string_lossy().into_owned()),
                            reason,
                        });
                    }
                    return Ok((0, false));
                }
                Err(_) => {
                    for path in candidates {
                        blocked.push(BlockedInterruptedPath {
                            path: Some(path.to_string_lossy().into_owned()),
                            reason: "artifact_use_inventory_unavailable",
                        });
                    }
                    return Ok((0, false));
                }
            };
        if check_deadline(Some(deadline)) {
            return Ok((0, false));
        }
        let current_cohort =
            self.closed_artifact_checkout_cohort_snapshot(owner_session_id, root, Some(deadline))?;
        if current_cohort.as_ref() != Some(expected_cohort) {
            for path in candidates {
                blocked.push(BlockedInterruptedPath {
                    path: Some(path.to_string_lossy().into_owned()),
                    reason: "closed_checkout_cohort_changed",
                });
            }
            return Ok((0, false));
        }
        if self
            .refuse_live_checkout_until(owner_session_id, root, deadline)
            .is_err()
        {
            for path in candidates {
                blocked.push(BlockedInterruptedPath {
                    path: Some(path.to_string_lossy().into_owned()),
                    reason: "live_checkout_or_alias_detected",
                });
            }
            return Ok((0, false));
        }
        let checkout_result = {
            let _git_deadline = crate::git::limit_git_until(deadline);
            GitRepo::discover(root)
        };
        let checkout = match checkout_result {
            Ok(checkout) => checkout,
            Err(_) => {
                for path in candidates {
                    blocked.push(BlockedInterruptedPath {
                        path: Some(path.to_string_lossy().into_owned()),
                        reason: "ignored_directory_git_discovery_unavailable",
                    });
                }
                return Ok((0, false));
            }
        };

        let _artifact_use_reclaimer = artifact_use_reclaimer;
        let mut entries_reclaimed = 0usize;
        let mut complete = true;
        for path in candidates {
            if check_deadline(Some(deadline)) {
                blocked.push(BlockedInterruptedPath {
                    path: Some(path.to_string_lossy().into_owned()),
                    reason: "removal_budget_expired_before_admission",
                });
                complete = false;
                break;
            }
            let Some(relative) = repo_relative(root, path) else {
                blocked.push(BlockedInterruptedPath {
                    path: Some(path.to_string_lossy().into_owned()),
                    reason: "path_not_repository_relative",
                });
                complete = false;
                continue;
            };
            let current_policy = crate::AutoCleanupPolicy::load(self.main_root()).ok();
            if current_policy.as_ref() != Some(cleanup_policy)
                || !ignored_directory_sweep_policy_allows_checkout(cleanup_policy, root)
            {
                blocked.push(BlockedInterruptedPath {
                    path: Some(path.to_string_lossy().into_owned()),
                    reason: "ignored_directory_policy_changed",
                });
                complete = false;
                continue;
            }
            let current_entries = {
                let _git_deadline = crate::git::limit_git_until(deadline);
                checkout.ignored_paths_readonly_until(deadline)
            };
            let current_entries = match current_entries {
                Ok(entries) => entries,
                Err(_) => {
                    blocked.push(BlockedInterruptedPath {
                        path: Some(path.to_string_lossy().into_owned()),
                        reason: "ignored_directory_listing_unavailable",
                    });
                    complete = false;
                    continue;
                }
            };
            if !ignored_listing_has_directory_root(&current_entries, &relative) {
                // A lost ignore match, protected path, symlink, or changed
                // root shape revokes this plan. Keep both data and a typed
                // queue witness for later review/revalidation.
                blocked.push(BlockedInterruptedPath {
                    path: Some(path.to_string_lossy().into_owned()),
                    reason: "ignored_directory_not_currently_opaque",
                });
                complete = false;
                continue;
            }
            match std::fs::symlink_metadata(path) {
                Ok(metadata)
                    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => {
                    blocked.push(BlockedInterruptedPath {
                        path: Some(path.to_string_lossy().into_owned()),
                        reason: "ignored_directory_root_replaced",
                    });
                    complete = false;
                    continue;
                }
                Err(_) => {
                    blocked.push(BlockedInterruptedPath {
                        path: Some(path.to_string_lossy().into_owned()),
                        reason: "ignored_directory_metadata_unavailable",
                    });
                    complete = false;
                    continue;
                }
            }
            if !*queue_valid {
                blocked.push(BlockedInterruptedPath {
                    path: Some(path.to_string_lossy().into_owned()),
                    reason: "ignored_directory_queue_unavailable",
                });
                complete = false;
                continue;
            }
            let path_key = path.to_string_lossy().into_owned();
            let resuming = queued_paths.iter().any(|queued| queued == &path_key);
            if !resuming {
                let mut updated_paths = queued_paths.clone();
                updated_paths.push(path_key.clone());
                if encode_interrupted_ignored_directories(&updated_paths).is_none()
                    || !persist_interrupted_ignored_directories(
                        self.store(),
                        namespace,
                        &updated_paths,
                    )?
                {
                    *queue_valid = false;
                    blocked.push(BlockedInterruptedPath {
                        path: Some(path_key),
                        reason: "ignored_directory_queue_update_unavailable",
                    });
                    complete = false;
                    continue;
                }
                *queued_paths = updated_paths;
            }
            let removal_result = {
                let mut final_revalidation_complete = false;
                let mut before_mutation = || {
                    #[cfg(test)]
                    tests::before_gc_artifact_removal_mutation(path)?;
                    if final_revalidation_complete {
                        return Ok(());
                    }
                    let policy_current = crate::AutoCleanupPolicy::load(self.main_root())
                        .map_err(|error| std::io::Error::other(error.to_string()))?;
                    if &policy_current != cleanup_policy
                        || !ignored_directory_sweep_policy_allows_checkout(&policy_current, root)
                    {
                        return Err(std::io::Error::other(
                            "ignored-directory cleanup policy changed before unlink",
                        ));
                    }
                    let current_checkout = {
                        let _git_deadline = crate::git::limit_git_until(deadline);
                        GitRepo::discover(root)
                    }
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                    if !expected_cohort.matches_checkout_instance(&current_checkout) {
                        return Err(std::io::Error::other(
                            "closed checkout root identity changed before unlink",
                        ));
                    }
                    let current_entries = {
                        let _git_deadline = crate::git::limit_git_until(deadline);
                        current_checkout.ignored_paths_readonly_until(deadline)
                    }
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                    if !ignored_listing_has_directory_root(&current_entries, &relative) {
                        return Err(std::io::Error::other(
                            "ignored directory is no longer a current opaque Git directory",
                        ));
                    }
                    match std::fs::symlink_metadata(path) {
                        Ok(metadata)
                            if metadata.file_type().is_dir()
                                && !metadata.file_type().is_symlink() => {}
                        _ => {
                            return Err(std::io::Error::other(
                                "ignored directory root changed before unlink",
                            ));
                        }
                    }
                    final_revalidation_complete = true;
                    Ok(())
                };
                remove_condemned_tree_with_progress_and_guard(
                    path,
                    None,
                    Some(deadline),
                    &mut before_mutation,
                )
            };
            match removal_result {
                Ok((TreeRemoval::Complete, count)) => {
                    entries_reclaimed = entries_reclaimed.saturating_add(count);
                    let mut updated_paths = queued_paths.clone();
                    updated_paths.retain(|queued| queued != &path_key);
                    match persist_interrupted_ignored_directories(
                        self.store(),
                        namespace,
                        &updated_paths,
                    ) {
                        Ok(true) => {
                            *queued_paths = updated_paths;
                            removed.push(path_key);
                        }
                        Ok(false) => {
                            *queue_valid = false;
                            blocked.push(BlockedInterruptedPath {
                                path: Some(path_key),
                                reason: "ignored_directory_queue_update_unavailable",
                            });
                            complete = false;
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok((TreeRemoval::Interrupted, count)) => {
                    entries_reclaimed = entries_reclaimed.saturating_add(count);
                    complete = false;
                }
                Err(error) => {
                    entries_reclaimed = entries_reclaimed.saturating_add(error.entries_reclaimed);
                    blocked.push(BlockedInterruptedPath {
                        path: Some(path_key),
                        reason: "ignored_directory_removal_unavailable",
                    });
                    complete = false;
                }
            }
        }
        Ok((entries_reclaimed, complete))
    }

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
        self.sweep_artifacts_autonomously_with_deadlines(
            policy,
            scope,
            deadline_override,
            deadline_override,
            deadline_override,
            ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET,
        )
    }

    #[cfg(test)]
    fn sweep_artifacts_autonomously_with_test_deadlines(
        &mut self,
        policy: &RetentionPolicy,
        scope: ArtifactSweepScope,
        discovery_deadline: Instant,
        removal_deadline: Instant,
    ) -> Result<(ArtifactSweepOutcome, usize), BrokerOpError> {
        self.sweep_artifacts_autonomously_with_deadlines(
            policy,
            scope,
            Some(discovery_deadline),
            Some(removal_deadline),
            None,
            ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET,
        )
    }

    #[cfg(test)]
    fn sweep_artifacts_autonomously_with_test_lane_deadlines(
        &mut self,
        policy: &RetentionPolicy,
        scope: ArtifactSweepScope,
        discovery_deadline: Instant,
        removal_deadline: Instant,
        checkout_cleanup_deadline: Instant,
    ) -> Result<(ArtifactSweepOutcome, usize), BrokerOpError> {
        self.sweep_artifacts_autonomously_with_deadlines(
            policy,
            scope,
            Some(discovery_deadline),
            Some(removal_deadline),
            Some(checkout_cleanup_deadline),
            ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET,
        )
    }

    #[cfg(test)]
    fn sweep_artifacts_autonomously_with_test_discovery_budget(
        &mut self,
        policy: &RetentionPolicy,
        scope: ArtifactSweepScope,
        entry_budget: usize,
    ) -> Result<(ArtifactSweepOutcome, usize), BrokerOpError> {
        self.sweep_artifacts_autonomously_with_deadlines(
            policy,
            scope,
            None,
            None,
            None,
            entry_budget,
        )
    }

    fn sweep_artifacts_autonomously_with_deadlines(
        &mut self,
        policy: &RetentionPolicy,
        scope: ArtifactSweepScope,
        discovery_deadline_override: Option<Instant>,
        removal_deadline_override: Option<Instant>,
        checkout_cleanup_deadline_override: Option<Instant>,
        discovery_entry_budget: usize,
    ) -> Result<(ArtifactSweepOutcome, usize), BrokerOpError> {
        let main_root = self.main_root().to_path_buf();
        let now = now_ms();
        let (budget_ms, urgency) = self.artifact_sweep_budget(policy, scope);
        // The total pass budget still covers both lanes. General sweeps reserve
        // a bounded tail for whole-checkout cleanup so cache discovery cannot
        // consume the entire foreground budget first.
        let budget_started_at = Instant::now();
        let inline_pass_spaced = if scope == ArtifactSweepScope::Inline {
            let interval_ms = (i64::from(policy.artifact_sweep_interval_hours) * 3_600_000)
                / urgency.interval_divisor().max(1);
            let spacing_ms = INLINE_PARTIAL_PASS_SPACING_MS / urgency.interval_divisor().max(1);
            self.meta_stamp_within(ARTIFACT_SWEEP_STAMP_KEY, now, interval_ms)?
                || self.meta_stamp_within(ARTIFACT_SWEEP_PROGRESS_KEY, now, spacing_ms)?
        } else {
            false
        };
        if inline_pass_spaced && policy.artifact_sweep_budget_ms != 0 {
            return Ok((
                ArtifactSweepOutcome {
                    directories_reclaimed: 0,
                    complete: true,
                    auto_cleanup: None,
                },
                0,
            ));
        }
        if policy.artifact_sweep_budget_ms == 0 {
            if scope_runs_checkout_cleanup(scope) {
                // The cache lane can be disabled while session retirement is
                // still enabled. Its single-row zero-timeout CAS can enqueue
                // without waiting behind the cache worker's lifecycle lock.
                crate::warn_unrecorded(
                    "enqueue bounded session-retirement request",
                    self.request_session_retirement_if_due(policy, scope),
                );
            }
            return Ok((
                ArtifactSweepOutcome {
                    directories_reclaimed: 0,
                    complete: true,
                    auto_cleanup: None,
                },
                0,
            ));
        }
        let namespace = self.ensure_artifact_sweep_namespace()?;
        let closed_session_id = match scope {
            ArtifactSweepScope::ClosedSession(session_id) => Some(session_id),
            ArtifactSweepScope::Inline
            | ArtifactSweepScope::Drain
            | ArtifactSweepScope::FinishDrain
            | ArtifactSweepScope::AutomaticContinuation => None,
        };
        // Avoid reading cleanup policy on broker opens whose inline pass is
        // already spaced out. The original start still anchors the total cap.
        let cleanup_policy_snapshot = crate::AutoCleanupPolicy::load(self.main_root()).ok();
        let auto_remove_enabled = scope_runs_checkout_cleanup(scope)
            && cleanup_policy_snapshot
                .as_ref()
                .is_some_and(|policy| policy.auto_remove);
        let ignored_directory_policy = cleanup_policy_snapshot
            .as_ref()
            .filter(|policy| policy.ignored_directory_sweep && !policy.strict)
            .cloned();
        let checkout_cleanup_reserve =
            foreground_checkout_cleanup_reserve(budget_ms, scope, auto_remove_enabled);
        let deadlines = artifact_sweep_deadlines(
            budget_started_at,
            budget_ms,
            checkout_cleanup_reserve,
            discovery_deadline_override,
            removal_deadline_override,
            checkout_cleanup_deadline_override,
        );
        let discovery_deadline = deadlines.discovery;
        let removal_deadline = deadlines.removal;
        let checkout_cleanup_deadline = deadlines.checkout_cleanup;
        let deadline = discovery_deadline.min(removal_deadline);
        // This is one zero-timeout CAS row update. It is counted inside the
        // existing sweep budget and can be admitted while another worker owns
        // GcLock for cache work.
        let session_retirement_request = match self.request_session_retirement_if_due(policy, scope)
        {
            Ok(request) => request,
            Err(error) => {
                crate::warn_unrecorded(
                    "enqueue bounded session-retirement request",
                    Err::<(), _>(error),
                );
                None
            }
        };
        if !crate::AutoCleanupPolicy::artifact_sweep_allowed(self.main_root()) {
            // Strict mode pauses the artifact lane without consuming its pending
            // generation. Ignored-free checkouts may still satisfy their own
            // cleanup policy, under the same bounded lock/proof discipline.
            let auto_cleanup = if scope_runs_checkout_cleanup(scope) {
                if let Ok(_lock) = GcLock::acquire(self.main_root()) {
                    let report =
                        self.auto_remove_disposable_checkouts(Some(checkout_cleanup_deadline))?;
                    self.persist_auto_checkout_cleanup_request(scope, &report)?;
                    if report.retirement_deferred_reasons.is_empty()
                        && let Some(request_generation) = session_retirement_request
                    {
                        crate::warn_unrecorded(
                            "complete abandoned-session retirement request",
                            self.complete_session_retirement_request(request_generation),
                        );
                    }
                    if scope == ArtifactSweepScope::Inline {
                        self.artifact_sweep_meta_set(
                            ARTIFACT_SWEEP_PROGRESS_KEY,
                            &now.to_string(),
                        )?;
                    }
                    Some(report)
                } else {
                    None
                }
            } else {
                None
            };
            return Ok((
                ArtifactSweepOutcome {
                    directories_reclaimed: 0,
                    complete: false,
                    auto_cleanup,
                },
                0,
            ));
        }
        // A concurrent GC owns the artifact namespace; skip rather than race.
        let Ok(_lock) = GcLock::acquire(&main_root) else {
            return Ok((
                ArtifactSweepOutcome {
                    directories_reclaimed: 0,
                    // An explicit sweep did not inspect the namespace and
                    // must remain retryable while another GC owns its lock.
                    complete: closed_session_id.is_none()
                        && !matches!(
                            scope,
                            ArtifactSweepScope::AutomaticContinuation | ArtifactSweepScope::Drain
                        ),
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
        let mut closed_cohort_index_failed = false;
        let closed_cohort_index_unresolved = if ignored_directory_policy.is_some() {
            match self.advance_cleanup_cohort_scan(Some(discovery_deadline))? {
                Some(state) if state.complete => {
                    closed_cohort_index_failed = state.unresolved_rows > 0;
                    state.unresolved_rows
                }
                Some(state) => {
                    closed_cohort_index_failed = true;
                    state.unresolved_rows
                }
                None => {
                    closed_cohort_index_failed = true;
                    0
                }
            }
        } else {
            0
        };
        let mut preparation_entries_reclaimed = 0usize;
        let mut scan_completed = !closed_cohort_index_failed;
        if closed_session_id.is_none() {
            // Cache roots are shared across repositories and clones, while the
            // selected worktree roots belong to this repository. Recover
            // witnessed rotations from independently validated bindings, but
            // do not infer a complete cache consumer set from this repo alone.
            let storage_context = self.storage_context().clone();
            let mut cache_bindings = Vec::new();
            let mut bindings_fully_enumerated = true;
            match storage_context.preparation_cache_root_binding() {
                Ok(Some(binding)) => cache_bindings.push(binding),
                Ok(None) => {}
                Err(_) => bindings_fully_enumerated = false,
            }
            match self.worktree_volume_preparation_cache_root_bindings() {
                Ok(bindings) => cache_bindings.extend(bindings),
                Err(_) => bindings_fully_enumerated = false,
            }
            cache_bindings.sort_by(|left, right| left.path.cmp(&right.path));
            cache_bindings.dedup_by(|left, right| left.path == right.path);
            let preparation_sweep = match storage_context.host_resource_db_path.as_deref() {
                Some(resource_db_path) => {
                    crate::storage::recover_preparation_caches_for_bindings(
                        &cache_bindings,
                        resource_db_path,
                        deadline,
                        bindings_fully_enumerated,
                    )
                }
                None => crate::storage::PreparationCacheSweepReport {
                    entries_removed: 0,
                    bytes_reclaimed: 0,
                    filesystem_entries_reclaimed: 0,
                    complete: false,
                    coverage_deferral: Some(
                        crate::storage::PreparationCacheCoverageDeferral::CacheBindingsNotFullyEnumerated,
                    ),
                },
            };
            preparation_entries_reclaimed = preparation_sweep.filesystem_entries_reclaimed;
            scan_completed &= preparation_sweep.complete;
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
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_CURSOR_KEY)?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let mut last_visited = cursor;
        let mut pages = ClosedSessionPages::new(cursor, closed_session_id);
        let mut sessions = std::collections::VecDeque::new();
        let interrupted_value = self.artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)?;
        let mut interrupted_paths_valid = true;
        let mut blocked_interrupted_paths = Vec::<BlockedInterruptedPath>::new();
        if closed_cohort_index_failed {
            blocked_interrupted_paths.push(BlockedInterruptedPath {
                path: None,
                reason: if closed_cohort_index_unresolved > 0 {
                    "closed_checkout_path_resolution_unavailable"
                } else {
                    "closed_checkout_cohort_index_unavailable"
                },
            });
        }
        let mut interrupted_paths = match decode_interrupted_paths(interrupted_value.as_deref()) {
            Ok(paths) => paths,
            Err(()) => {
                // Preserve malformed or over-cap state as a durable blocker.
                // Do not delete new trees while we cannot prove the queue has
                // room to remember a partial removal.
                interrupted_paths_valid = false;
                scan_completed = false;
                blocked_interrupted_paths.push(BlockedInterruptedPath {
                    path: None,
                    reason: "queue_invalid_or_oversized",
                });
                Vec::new()
            }
        };
        let ignored_directory_queue_value =
            self.artifact_sweep_meta_get(ARTIFACT_SWEEP_IGNORED_DIR_INTERRUPTED_KEY)?;
        let mut ignored_directory_queue_valid = true;
        let mut ignored_directory_paths = match decode_interrupted_ignored_directories(
            ignored_directory_queue_value.as_deref(),
        ) {
            Ok(paths) => paths,
            Err(()) => {
                ignored_directory_queue_valid = false;
                scan_completed = false;
                blocked_interrupted_paths.push(BlockedInterruptedPath {
                    path: None,
                    reason: "ignored_directory_queue_invalid_or_oversized",
                });
                Vec::new()
            }
        };
        if ignored_directory_queue_valid {
            let previous_paths = ignored_directory_paths.clone();
            let unavailable = prune_absent_interrupted_paths(&mut ignored_directory_paths);
            if !unavailable.is_empty() {
                scan_completed = false;
                blocked_interrupted_paths.extend(unavailable.into_iter().map(|path| {
                    BlockedInterruptedPath {
                        path: Some(path),
                        reason: "ignored_directory_metadata_unavailable",
                    }
                }));
            }
            if ignored_directory_paths != previous_paths
                && !persist_interrupted_ignored_directories(
                    self.store(),
                    &namespace,
                    &ignored_directory_paths,
                )?
            {
                ignored_directory_queue_valid = false;
                scan_completed = false;
                blocked_interrupted_paths.push(BlockedInterruptedPath {
                    path: None,
                    reason: "ignored_directory_queue_update_unavailable",
                });
            }
        }
        if !ignored_directory_paths.is_empty() && ignored_directory_policy.is_none() {
            scan_completed = false;
            blocked_interrupted_paths.extend(ignored_directory_paths.iter().map(|path| {
                BlockedInterruptedPath {
                    path: Some(path.clone()),
                    reason: "ignored_directory_sweep_disabled",
                }
            }));
        }
        let mut revoked_interrupted_paths = Vec::<RevokedInterruptedPath>::new();
        if interrupted_paths_valid {
            // A queued path containing `..` can never be a deletion witness.
            // Retire it before checking liveness or touching the referenced
            // filesystem path, so a malformed marker cannot poison recovery
            // while its session remains open.
            let structurally_revoked = interrupted_paths
                .iter()
                .filter(|path| has_parent_dir_component(Path::new(path)))
                .cloned()
                .collect::<Vec<_>>();
            if !structurally_revoked.is_empty() {
                let mut updated_paths = interrupted_paths.clone();
                updated_paths.retain(|path| !structurally_revoked.contains(path));
                if persist_interrupted_paths(self.store(), &namespace, &updated_paths)? {
                    interrupted_paths = updated_paths;
                    revoked_interrupted_paths.extend(structurally_revoked.into_iter().map(
                        |path| RevokedInterruptedPath {
                            path,
                            reason: "unsafe_path_components",
                            canonical_path: None,
                        },
                    ));
                } else {
                    interrupted_paths_valid = false;
                    scan_completed = false;
                    blocked_interrupted_paths.extend(structurally_revoked.into_iter().map(
                        |path| BlockedInterruptedPath {
                            path: Some(path),
                            reason: "queue_update_unavailable",
                        },
                    ));
                }
            }
        }
        if interrupted_paths_valid {
            let previous_paths = interrupted_paths.clone();
            let unavailable = prune_absent_interrupted_paths(&mut interrupted_paths);
            if !unavailable.is_empty() {
                scan_completed = false;
                blocked_interrupted_paths.extend(unavailable.into_iter().map(|path| {
                    BlockedInterruptedPath {
                        path: Some(path),
                        reason: "metadata_unavailable",
                    }
                }));
            }
            if interrupted_paths != previous_paths
                && !persist_interrupted_paths(self.store(), &namespace, &interrupted_paths)?
            {
                interrupted_paths_valid = false;
                scan_completed = false;
                blocked_interrupted_paths.push(BlockedInterruptedPath {
                    path: None,
                    reason: "queue_update_unavailable",
                });
            }
        }
        blocked_interrupted_paths.extend(
            interrupted_paths
                .iter()
                .filter(|path| {
                    let path = Path::new(path);
                    live_worktrees.iter().any(|root| path.starts_with(root))
                })
                .map(|path| BlockedInterruptedPath {
                    path: Some(path.clone()),
                    reason: "live_session",
                }),
        );
        let mut visited_any = false;
        loop {
            // Checked before the session is taken, so the cursor names the
            // last session actually visited and the next pass starts here.
            // The first session is always visited: a budget spent before the
            // loop must still buy ground, or a backlog never converges.
            if visited_any && check_deadline(Some(discovery_deadline)) {
                scan_completed = false;
                break;
            }
            if sessions.is_empty() {
                sessions.extend(pages.next_page(self.store())?);
            }
            let Some(session) = sessions.pop_front() else {
                break;
            };
            visited_any = true;
            last_visited = session.id;
            let mut session_scan_complete = true;
            let root = PathBuf::from(&session.worktree_path);
            if live.contains(&session.id) {
                continue;
            }
            let closed_at = session.closed_at.unwrap_or(session.updated_at);
            if days_between(now, closed_at) < policy.artifact_reclaim_days {
                continue;
            }
            let sheltered = overlaps_live_worktree(&root, &live_worktrees);
            if sheltered {
                blocked_interrupted_paths.push(BlockedInterruptedPath {
                    path: Some(root.to_string_lossy().into_owned()),
                    reason: "live_session",
                });
                // Retention is a deferral, not completion: revisit this
                // closed checkout after its live alias releases ownership.
                scan_completed = false;
                continue;
            }
            let expected_ignored_cohort = if ignored_directory_policy.is_some() {
                let cohort = if closed_cohort_index_failed {
                    None
                } else {
                    self.closed_artifact_checkout_cohort_for_session(
                        session.id,
                        &root,
                        Some(discovery_deadline),
                    )?
                };
                if cohort
                    .as_ref()
                    .is_some_and(|cohort| cohort.owner_session_id() != session.id)
                {
                    // The canonical group is processed once at its spawned
                    // owner row, regardless of how many closed aliases name it.
                    continue;
                }
                cohort
            } else {
                None
            };
            match std::fs::symlink_metadata(&root) {
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Ok(_) => {
                    // A present non-directory root may be a replacement or a
                    // symlink. Keep interrupted intent and never follow it.
                    blocked_interrupted_paths.push(BlockedInterruptedPath {
                        path: Some(root.to_string_lossy().into_owned()),
                        reason: "worktree_root_replaced",
                    });
                    scan_completed = false;
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // An absent worktree proves its queued descendants are
                    // absent too. Retire only those intents; no replacement
                    // path is opened or removed.
                    let mut sweep_writer = ScopedArtifactSweepWriter {
                        store: self.store(),
                        namespace: &namespace,
                    };
                    if !revoke_interrupted_paths_under_root(
                        &mut sweep_writer,
                        &mut interrupted_paths,
                        &mut interrupted_paths_valid,
                        &root,
                        "worktree_root_absent",
                        &mut revoked_interrupted_paths,
                        &mut blocked_interrupted_paths,
                    )? {
                        scan_completed = false;
                    }
                    continue;
                }
                Err(_) => {
                    blocked_interrupted_paths.push(BlockedInterruptedPath {
                        path: Some(root.to_string_lossy().into_owned()),
                        reason: "worktree_metadata_unavailable",
                    });
                    scan_completed = false;
                    continue;
                }
            }
            let ownership = {
                let _git_deadline = crate::git::limit_git_until(discovery_deadline);
                self.is_broker_owned_worktree_checked(&session, &root)
            };
            match ownership {
                Ok(true) => {}
                Ok(false) => {
                    let mut sweep_writer = ScopedArtifactSweepWriter {
                        store: self.store(),
                        namespace: &namespace,
                    };
                    if !revoke_interrupted_paths_under_root(
                        &mut sweep_writer,
                        &mut interrupted_paths,
                        &mut interrupted_paths_valid,
                        &root,
                        "worktree_ownership_revoked",
                        &mut revoked_interrupted_paths,
                        &mut blocked_interrupted_paths,
                    )? {
                        scan_completed = false;
                    }
                    continue;
                }
                Err(_) => {
                    blocked_interrupted_paths.push(BlockedInterruptedPath {
                        path: Some(root.to_string_lossy().into_owned()),
                        reason: "worktree_ownership_unavailable",
                    });
                    scan_completed = false;
                    continue;
                }
            }
            let checkout_result = {
                let _git_deadline = crate::git::limit_git_until(discovery_deadline);
                GitRepo::discover(&root)
            };
            let checkout = match checkout_result {
                Ok(checkout) => checkout,
                Err(_) => {
                    // Git discovery is part of the proof that the candidate
                    // belongs to this checkout. A timeout or any other
                    // failure leaves the session unclassified, so retain the
                    // unfinished pass, then let later sessions make progress.
                    blocked_interrupted_paths.push(BlockedInterruptedPath {
                        path: Some(root.to_string_lossy().into_owned()),
                        reason: "git_discovery_unavailable",
                    });
                    scan_completed = false;
                    continue;
                }
            };
            eligible_worktree_seen = true;
            let mut ignored_directory_discovery = None;
            let mut ignored_directory_candidates = BTreeSet::new();
            let mut ignored_directory_discovery_complete = true;
            if let Some(cleanup_policy) = ignored_directory_policy.as_ref() {
                match discover_ignored_directory_roots_bounded(
                    &root,
                    &checkout,
                    &policy.artefact_directories,
                    discovery_deadline,
                    discovery_entry_budget,
                ) {
                    Ok(discovery) => {
                        ignored_directory_discovery_complete = discovery.complete;
                        if expected_ignored_cohort.is_some()
                            && ignored_directory_sweep_policy_allows_checkout(cleanup_policy, &root)
                        {
                            ignored_directory_candidates
                                .extend(discovery.generic_roots.iter().cloned());
                            for queued in &ignored_directory_paths {
                                let queued_path = PathBuf::from(queued);
                                if !queued_path.starts_with(&root) {
                                    continue;
                                }
                                let relative = repo_relative(&root, &queued_path);
                                if relative.as_deref().is_some_and(|relative| {
                                    ignored_listing_has_directory_root(&discovery.entries, relative)
                                }) {
                                    ignored_directory_candidates.insert(queued_path);
                                } else if std::fs::symlink_metadata(&queued_path).is_ok() {
                                    blocked_interrupted_paths.push(BlockedInterruptedPath {
                                        path: Some(queued.clone()),
                                        reason: "ignored_directory_not_currently_opaque",
                                    });
                                    ignored_directory_discovery_complete = false;
                                }
                            }
                        } else if !discovery.generic_roots.is_empty() {
                            let reason = if expected_ignored_cohort.is_some() {
                                "checkout_pinned_by_cleanup_policy"
                            } else {
                                "closed_checkout_cohort_unavailable"
                            };
                            for path in &discovery.generic_roots {
                                blocked_interrupted_paths.push(BlockedInterruptedPath {
                                    path: Some(path.to_string_lossy().into_owned()),
                                    reason,
                                });
                            }
                            ignored_directory_discovery_complete = false;
                        }
                        ignored_directory_discovery = Some(discovery);
                    }
                    Err(()) => {
                        blocked_interrupted_paths.push(BlockedInterruptedPath {
                            path: Some(root.to_string_lossy().into_owned()),
                            reason: "ignored_directory_listing_unavailable",
                        });
                        ignored_directory_discovery_complete = false;
                        session_scan_complete = false;
                    }
                }
            }
            let discovery = match ignored_directory_discovery.as_ref() {
                Some(ignored) => collect_artifact_dirs_bounded_with_opaque_ignored_roots(
                    &root,
                    &policy.artefact_directories,
                    &ignored.opaque_roots,
                    discovery_deadline,
                    discovery_entry_budget.saturating_sub(ignored.entries_examined),
                ),
                None if ignored_directory_policy.is_some() => BoundedArtifactDiscovery {
                    classified: Vec::new(),
                    complete: false,
                    entries_examined: 0,
                },
                None => collect_artifact_dirs_bounded(
                    &root,
                    &policy.artefact_directories,
                    discovery_deadline,
                    discovery_entry_budget,
                ),
            };
            let discovery_complete = discovery.complete && ignored_directory_discovery_complete;
            let mut found = discovery.classified;
            // Interrupted trees are injected ahead of newly discovered caches:
            // after partial removal their original witness may be gone, but
            // their exact path remains safe to resume after fresh validation.
            let mut pending_in_worktree = Vec::new();
            let mut revoked_in_worktree = Vec::new();
            for path in interrupted_paths
                .iter()
                .map(|path| PathBuf::from(path.as_str()))
                .filter(|path| path.starts_with(&root))
            {
                match interrupted_tree_state(&root, &path, &policy.artefact_directories) {
                    InterruptedTreeState::Resume => pending_in_worktree.push(path),
                    InterruptedTreeState::Revoked(reason) => {
                        if interrupted_paths_valid {
                            let path = path.to_string_lossy().into_owned();
                            revoked_in_worktree.push(RevokedInterruptedPath {
                                canonical_path: std::fs::canonicalize(&path).ok(),
                                path,
                                reason,
                            });
                        } else {
                            blocked_interrupted_paths.push(BlockedInterruptedPath {
                                path: Some(path.to_string_lossy().into_owned()),
                                reason: "queue_unavailable_for_revocation",
                            });
                            session_scan_complete = false;
                        }
                    }
                    InterruptedTreeState::Blocked(reason) => {
                        blocked_interrupted_paths.push(BlockedInterruptedPath {
                            path: Some(path.to_string_lossy().into_owned()),
                            reason,
                        });
                        session_scan_complete = false;
                    }
                    InterruptedTreeState::Unavailable(reason) => {
                        // Without a trustworthy queue write, or when the
                        // filesystem cannot be inspected, preserve the marker
                        // and keep the pass incomplete.
                        blocked_interrupted_paths.push(BlockedInterruptedPath {
                            path: Some(path.to_string_lossy().into_owned()),
                            reason,
                        });
                        session_scan_complete = false;
                    }
                }
            }
            let revoked_targets = revoked_in_worktree
                .iter()
                .filter_map(|revoked| revoked.canonical_path.clone())
                .collect::<BTreeSet<_>>();
            let revoked_paths = revoked_in_worktree
                .iter()
                .map(|revoked| PathBuf::from(&revoked.path))
                .collect::<BTreeSet<_>>();
            if !revoked_in_worktree.is_empty() {
                let revoked_paths = revoked_in_worktree
                    .iter()
                    .map(|revoked| revoked.path.as_str())
                    .collect::<BTreeSet<_>>();
                let mut updated_paths = interrupted_paths.clone();
                updated_paths.retain(|path| !revoked_paths.contains(path.as_str()));
                if persist_interrupted_paths(self.store(), &namespace, &updated_paths)? {
                    interrupted_paths = updated_paths;
                    revoked_interrupted_paths.extend(revoked_in_worktree);
                } else {
                    interrupted_paths_valid = false;
                    session_scan_complete = false;
                    blocked_interrupted_paths.extend(revoked_in_worktree.iter().map(|revoked| {
                        BlockedInterruptedPath {
                            path: Some(revoked.path.clone()),
                            reason: "queue_update_unavailable",
                        }
                    }));
                }
            }
            found.retain(|dir| !pending_in_worktree.contains(dir));
            found.retain(|dir| !revoked_paths.contains(dir));
            found.retain(|dir| {
                std::fs::canonicalize(dir)
                    .ok()
                    .is_none_or(|canonical| !revoked_targets.contains(&canonical))
            });
            pending_in_worktree.extend(found);
            let mut found = pending_in_worktree;
            if !ignored_directory_candidates.is_empty() {
                found.retain(|dir| {
                    !ignored_directory_candidates
                        .iter()
                        .any(|ignored_root| dir.starts_with(ignored_root))
                });
                if let Some(cleanup_policy) = ignored_directory_policy.as_ref() {
                    let (reclaimed, ignored_complete) = self
                        .reclaim_ignored_directory_roots_under_claims(
                            &root,
                            expected_ignored_cohort.as_ref(),
                            &ignored_directory_candidates,
                            &namespace,
                            &mut ignored_directory_paths,
                            &mut ignored_directory_queue_valid,
                            cleanup_policy,
                            removal_deadline,
                            &mut blocked_interrupted_paths,
                            &mut removed,
                        )?;
                    entries_reclaimed = entries_reclaimed.saturating_add(reclaimed);
                    if !ignored_complete {
                        session_scan_complete = false;
                    }
                }
            }
            for dir in found {
                let Some(relative) = repo_relative(&root, &dir) else {
                    blocked_interrupted_paths.push(BlockedInterruptedPath {
                        path: Some(dir.to_string_lossy().into_owned()),
                        reason: "path_not_repository_relative",
                    });
                    session_scan_complete = false;
                    continue;
                };
                let ignored = {
                    let _git_deadline = crate::git::limit_git_until(discovery_deadline);
                    checkout.path_is_ignored_checked(&relative)
                };
                let ignored = match ignored {
                    Ok(ignored) => ignored,
                    Err(_) => {
                        // A Git failure (including a shorter configured Git
                        // timeout) leaves this candidate unclassified. Do not
                        // advance the recovery generation on that basis.
                        blocked_interrupted_paths.push(BlockedInterruptedPath {
                            path: Some(dir.to_string_lossy().into_owned()),
                            reason: "git_ignore_check_unavailable",
                        });
                        session_scan_complete = false;
                        break;
                    }
                };
                let dir_key = dir.to_string_lossy().into_owned();
                let resuming = interrupted_paths.iter().any(|path| path == &dir_key);
                if !ignored {
                    if resuming {
                        if interrupted_paths_valid {
                            let mut updated_paths = interrupted_paths.clone();
                            updated_paths.retain(|path| path != &dir_key);
                            if persist_interrupted_paths(self.store(), &namespace, &updated_paths)?
                            {
                                interrupted_paths = updated_paths;
                                revoked_interrupted_paths.push(RevokedInterruptedPath {
                                    canonical_path: std::fs::canonicalize(&dir).ok(),
                                    path: dir_key,
                                    reason: "no_longer_git_ignored",
                                });
                            } else {
                                interrupted_paths_valid = false;
                                blocked_interrupted_paths.push(BlockedInterruptedPath {
                                    path: Some(dir_key),
                                    reason: "queue_update_unavailable",
                                });
                                session_scan_complete = false;
                            }
                        } else {
                            session_scan_complete = false;
                        }
                    }
                    continue;
                }
                if !interrupted_paths_valid {
                    session_scan_complete = false;
                    continue;
                }
                // A strict-policy switch revokes automatic deletion, including
                // resuming a previously authorized partial artifact removal.
                if !crate::AutoCleanupPolicy::artifact_sweep_allowed(self.main_root()) {
                    blocked_interrupted_paths.push(BlockedInterruptedPath {
                        path: Some(dir_key),
                        reason: "cleanup_policy_requires_manual_removal",
                    });
                    session_scan_complete = false;
                    continue;
                }
                // These earlier checks remain the cleanup authority. The
                // reclaimer is an additional exclusion for cooperating B1
                // producers; unknown markers and unsupported targets retain
                // the cache. Check both sides of its bounded filesystem work
                // so it cannot start deletion after the sweep budget expires.
                if check_deadline(Some(removal_deadline)) {
                    blocked_interrupted_paths.push(BlockedInterruptedPath {
                        path: Some(dir_key),
                        reason: "removal_budget_expired_before_admission",
                    });
                    session_scan_complete = false;
                    continue;
                }
                let _lifecycle_claim = match self.try_claim_worktree_lifecycle() {
                    Ok(Some(claim)) => claim,
                    Ok(None) => {
                        blocked_interrupted_paths.push(BlockedInterruptedPath {
                            path: Some(dir_key),
                            reason: "worktree_lifecycle_contended",
                        });
                        session_scan_complete = false;
                        continue;
                    }
                    Err(_) => {
                        blocked_interrupted_paths.push(BlockedInterruptedPath {
                            path: Some(dir_key),
                            reason: "worktree_lifecycle_unavailable",
                        });
                        session_scan_complete = false;
                        continue;
                    }
                };
                let current_session = self.store().session(session.id)?;
                if !current_session.status.is_closed()
                    || current_session.cleanup_state != crate::SessionCleanupState::Closed
                    || current_session.worktree_path != session.worktree_path
                    || current_session.branch != session.branch
                    || current_session.origin != session.origin
                    || current_session.updated_at != session.updated_at
                {
                    blocked_interrupted_paths.push(BlockedInterruptedPath {
                        path: Some(dir_key),
                        reason: "closed_checkout_session_changed",
                    });
                    session_scan_complete = false;
                    continue;
                }
                let owned = {
                    let _git_deadline = crate::git::limit_git_until(removal_deadline);
                    self.is_broker_owned_worktree_checked(&current_session, &root)
                };
                if !matches!(owned, Ok(true))
                    || self
                        .refuse_live_checkout_until(session.id, &root, removal_deadline)
                        .is_err()
                {
                    blocked_interrupted_paths.push(BlockedInterruptedPath {
                        path: Some(dir_key),
                        reason: "closed_checkout_ownership_or_live_alias_changed",
                    });
                    session_scan_complete = false;
                    continue;
                }
                let artifact_use_reclaimer =
                    match crate::file_lock::artifact_use::try_artifact_use_reclaimer(
                        &main_root, &root,
                    ) {
                        Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(
                            reclaimer,
                        )) => reclaimer,
                        Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(
                            block,
                        )) => {
                            let reason = match block {
                            crate::file_lock::artifact_use::ArtifactUseReclaimBlock::Contended => {
                                "artifact_use_contended"
                            }
                            crate::file_lock::artifact_use::ArtifactUseReclaimBlock::UnknownProducer {
                                ..
                            } => "artifact_use_unknown_producer",
                            crate::file_lock::artifact_use::ArtifactUseReclaimBlock::ProtocolUnavailable => {
                                "artifact_use_protocol_unavailable"
                            }
                        };
                            blocked_interrupted_paths.push(BlockedInterruptedPath {
                                path: Some(dir_key),
                                reason,
                            });
                            session_scan_complete = false;
                            continue;
                        }
                        Err(_) => {
                            blocked_interrupted_paths.push(BlockedInterruptedPath {
                                path: Some(dir_key),
                                reason: "artifact_use_inventory_unavailable",
                            });
                            session_scan_complete = false;
                            continue;
                        }
                    };
                if check_deadline(Some(removal_deadline)) {
                    blocked_interrupted_paths.push(BlockedInterruptedPath {
                        path: Some(dir_key),
                        reason: "removal_budget_expired_after_admission",
                    });
                    session_scan_complete = false;
                    drop(artifact_use_reclaimer);
                    continue;
                }
                // Keep the exclusive reservation alive through the durable
                // interrupted-path update below, then release it before the
                // next checkout key is considered.
                let _artifact_use_reclaimer = artifact_use_reclaimer;
                if !resuming {
                    let mut updated_paths = interrupted_paths.clone();
                    updated_paths.push(dir_key.clone());
                    if encode_interrupted_paths(&updated_paths).is_none() {
                        // The queue is deliberately finite. Leave this cache
                        // intact and let a later pass retry after older paths
                        // have completed; never unlink without a recovery key.
                        blocked_interrupted_paths.push(BlockedInterruptedPath {
                            path: Some(dir_key),
                            reason: "queue_capacity_exceeded",
                        });
                        session_scan_complete = false;
                        continue;
                    }
                    if !persist_interrupted_paths(self.store(), &namespace, &updated_paths)? {
                        blocked_interrupted_paths.push(BlockedInterruptedPath {
                            path: Some(dir_key),
                            reason: "queue_update_unavailable",
                        });
                        session_scan_complete = false;
                        continue;
                    }
                    interrupted_paths = updated_paths;
                }
                let deferrable =
                    artifact_witness_for_with_extras(&dir, &policy.artefact_directories)
                        .and_then(ArtifactWitness::deferrable_entry);
                let removal_result = {
                    let mut before_mutation = || {
                        #[cfg(test)]
                        {
                            tests::before_gc_artifact_removal_mutation(&dir)
                        }
                        #[cfg(not(test))]
                        {
                            Ok(())
                        }
                    };
                    remove_condemned_tree_with_progress_and_guard(
                        &dir,
                        deferrable,
                        Some(removal_deadline),
                        &mut before_mutation,
                    )
                };
                match removal_result {
                    Ok((TreeRemoval::Complete, count)) => {
                        entries_reclaimed = entries_reclaimed.saturating_add(count);
                        let mut updated_paths = interrupted_paths.clone();
                        updated_paths.retain(|path| path != &dir_key);
                        match persist_interrupted_paths(self.store(), &namespace, &updated_paths) {
                            Ok(true) => interrupted_paths = updated_paths,
                            Ok(false) | Err(_) => {
                                // The cache is gone, but retain the old queue
                                // entry and retry its absence proof next time.
                                blocked_interrupted_paths.push(BlockedInterruptedPath {
                                    path: Some(dir_key.clone()),
                                    reason: "queue_update_unavailable",
                                });
                                session_scan_complete = false;
                            }
                        }
                        removed.push(dir_key);
                    }
                    // The path was durably queued before any unlink. The next
                    // pass will retry it even if removal consumed its witness.
                    Ok((TreeRemoval::Interrupted, count)) => {
                        entries_reclaimed = entries_reclaimed.saturating_add(count);
                        session_scan_complete = false;
                    }
                    Err(error) => {
                        entries_reclaimed =
                            entries_reclaimed.saturating_add(error.entries_reclaimed);
                        blocked_interrupted_paths.push(BlockedInterruptedPath {
                            path: Some(dir_key),
                            reason: "removal_unavailable",
                        });
                        session_scan_complete = false;
                    }
                }
                if check_deadline(Some(discovery_deadline))
                    || check_deadline(Some(removal_deadline))
                {
                    session_scan_complete = false;
                    break;
                }
            }
            if !discovery_complete {
                // This worktree is not fully classified. Keep the pass
                // explicitly incomplete, but rotate beyond it so a broad
                // early session cannot starve every later closed session.
                session_scan_complete = false;
            }
            if !session_scan_complete {
                scan_completed = false;
            }
        }
        // A remembered path outside every known session root no longer has
        // broker deletion authority. Retire just that marker; never follow or
        // remove the external path itself. Include live roots so a cache that
        // became active remains queued for a later closed-session pass.
        if interrupted_paths_valid && !interrupted_paths.is_empty() {
            let known_worktrees = self
                .store()
                .session_worktree_paths()?
                .into_iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>();
            let outside_managed_worktrees = interrupted_paths
                .iter()
                .filter(|path| {
                    let path = Path::new(path);
                    !known_worktrees.iter().any(|root| path.starts_with(root))
                })
                .cloned()
                .collect::<BTreeSet<_>>();
            if !outside_managed_worktrees.is_empty() {
                let mut updated_paths = interrupted_paths.clone();
                updated_paths.retain(|path| !outside_managed_worktrees.contains(path));
                if persist_interrupted_paths(self.store(), &namespace, &updated_paths)? {
                    interrupted_paths = updated_paths;
                    revoked_interrupted_paths.extend(outside_managed_worktrees.into_iter().map(
                        |path| RevokedInterruptedPath {
                            path,
                            reason: "outside_managed_worktrees",
                            canonical_path: None,
                        },
                    ));
                } else {
                    interrupted_paths_valid = false;
                    scan_completed = false;
                    blocked_interrupted_paths.extend(outside_managed_worktrees.iter().map(
                        |path| BlockedInterruptedPath {
                            path: Some(path.clone()),
                            reason: "queue_update_unavailable",
                        },
                    ));
                }
            }
        }
        if ignored_directory_queue_valid && !ignored_directory_paths.is_empty() {
            let known_worktrees = self
                .store()
                .session_worktree_paths()?
                .into_iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>();
            let outside_managed_worktrees = ignored_directory_paths
                .iter()
                .filter(|path| {
                    let path = Path::new(path);
                    !known_worktrees.iter().any(|root| path.starts_with(root))
                })
                .cloned()
                .collect::<BTreeSet<_>>();
            if !outside_managed_worktrees.is_empty() {
                let mut updated_paths = ignored_directory_paths.clone();
                updated_paths.retain(|path| !outside_managed_worktrees.contains(path));
                if persist_interrupted_ignored_directories(
                    self.store(),
                    &namespace,
                    &updated_paths,
                )? {
                    ignored_directory_paths = updated_paths;
                    revoked_interrupted_paths.extend(outside_managed_worktrees.into_iter().map(
                        |path| RevokedInterruptedPath {
                            path,
                            reason: "outside_managed_worktrees",
                            canonical_path: None,
                        },
                    ));
                } else {
                    ignored_directory_queue_valid = false;
                    scan_completed = false;
                    blocked_interrupted_paths.extend(outside_managed_worktrees.iter().map(
                        |path| BlockedInterruptedPath {
                            path: Some(path.clone()),
                            reason: "ignored_directory_queue_update_unavailable",
                        },
                    ));
                }
            }
        }
        if !ignored_directory_paths.is_empty() {
            scan_completed = false;
        }
        if ignored_directory_queue_valid
            && !persist_interrupted_ignored_directories(
                self.store(),
                &namespace,
                &ignored_directory_paths,
            )?
        {
            scan_completed = false;
        }
        if !interrupted_paths.is_empty() {
            // A full directory lap is not complete while a path remains
            // blocked or incompletely removed, including a path from a
            // session not visited in this bounded pass.
            scan_completed = false;
        }
        if interrupted_paths_valid
            && !persist_interrupted_paths(self.store(), &namespace, &interrupted_paths)?
        {
            scan_completed = false;
        }
        // Only a completed lap consumes the cadence window. Incomplete work
        // keeps the durable request/cursor pending; inline spacing limits how
        // often ordinary broker opens retry a blocked path.
        if let Some(session_id) = closed_session_id {
            if !scan_completed {
                // Retry this session first on the next ordinary broker open.
                self.artifact_sweep_meta_set(
                    ARTIFACT_SWEEP_CURSOR_KEY,
                    &session_id.saturating_sub(1).to_string(),
                )?;
                self.artifact_sweep_meta_set(ARTIFACT_SWEEP_STAMP_KEY, "0")?;
            }
        } else {
            if eligible_worktree_seen && scan_completed {
                self.artifact_sweep_meta_set(ARTIFACT_SWEEP_STAMP_KEY, &now.to_string())?;
            }
            // A finished lap starts the next one from the top; an interrupted pass
            // remembers where it stopped so the next one advances instead of
            // re-walking what it already cleared, and when, so broker opens
            // space their continuations instead of each paying a budget.
            if scan_completed {
                self.artifact_sweep_meta_set(ARTIFACT_SWEEP_CURSOR_KEY, "0")?;
                self.artifact_sweep_meta_set(ARTIFACT_SWEEP_PROGRESS_KEY, "0")?;
            } else {
                self.artifact_sweep_meta_set(ARTIFACT_SWEEP_CURSOR_KEY, &last_visited.to_string())?;
                self.artifact_sweep_meta_set(ARTIFACT_SWEEP_PROGRESS_KEY, &now.to_string())?;
            }
        }
        blocked_interrupted_paths.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.reason.cmp(right.reason))
        });
        blocked_interrupted_paths.dedup();
        let omitted_blocked_diagnostics = blocked_interrupted_paths.len()
            > ARTIFACT_SWEEP_BLOCKED_DIAGNOSTICS_MAX.saturating_sub(1);
        let mut blocked = blocked_interrupted_paths
            .iter()
            .take(ARTIFACT_SWEEP_BLOCKED_DIAGNOSTICS_MAX.saturating_sub(1))
            .map(|blocked| {
                serde_json::json!({
                    "path": blocked.path.as_deref().map(bounded_artifact_diagnostic_path),
                    "reason": blocked.reason,
                })
            })
            .collect::<Vec<_>>();
        if omitted_blocked_diagnostics {
            blocked.push(serde_json::json!({
                "path": null,
                "reason": "additional_blockers_omitted",
            }));
        }
        let blocked_json = serde_json::Value::Array(blocked.clone()).to_string();
        let blocked_state = if blocked_interrupted_paths.is_empty() {
            String::new()
        } else {
            blocked_json.clone()
        };
        let previous_blocked_state =
            self.artifact_sweep_meta_get(ARTIFACT_SWEEP_BLOCKED_PATHS_KEY)?;
        let blocked_state_changed =
            previous_blocked_state.as_deref().unwrap_or("") != blocked_state;
        if !removed.is_empty() || !revoked_interrupted_paths.is_empty() || blocked_state_changed {
            let mut revoked_interrupted_paths = revoked_interrupted_paths;
            revoked_interrupted_paths.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| left.reason.cmp(right.reason))
            });
            revoked_interrupted_paths.dedup();
            let revoked = revoked_interrupted_paths
                .iter()
                .map(|revoked| {
                    serde_json::json!({
                        "path": bounded_artifact_diagnostic_path(&revoked.path),
                        "reason": revoked.reason,
                    })
                })
                .collect::<Vec<_>>();
            let payload = serde_json::json!({
                "directories": removed.len(),
                "idle_days": policy.artifact_reclaim_days,
                "closed_session_directories": removed.len(),
                "idle_open_session_directories": 0,
                "idle_session_artifact_hours": policy.idle_session_artifact_hours,
                "complete": scan_completed,
                "revoked_interrupted_paths": revoked,
                "blocked_interrupted_paths": blocked,
            })
            .to_string();
            self.store().append_event(
                crate::events::BROKER_GC_ARTIFACTS_SWEPT,
                None,
                Some(&payload),
            )?;
        }
        if blocked_state_changed {
            self.artifact_sweep_meta_set(ARTIFACT_SWEEP_BLOCKED_PATHS_KEY, &blocked_state)?;
        }
        // Whole checkouts go after their caches, under the same lock and
        // budget, and only on the general sweep: the targeted pass after a
        // close is about one session's build output (#588). Its failure must
        // not stop the sweep any more than the sweep's may stop a command.
        let auto_cleanup = if closed_session_id.is_none() && scope_runs_checkout_cleanup(scope) {
            match self.auto_remove_disposable_checkouts(Some(checkout_cleanup_deadline)) {
                Ok(report) => {
                    // This runs under the sweep's GC lock. A detached cleanup
                    // pass holds that same lock through generation completion,
                    // so an ordinary deferral cannot race a stale completion.
                    crate::warn_unrecorded(
                        "persist deferred automatic checkout cleanup request",
                        self.persist_auto_checkout_cleanup_request(scope, &report),
                    );
                    if report.retirement_deferred_reasons.is_empty()
                        && let Some(request_generation) = session_retirement_request
                    {
                        crate::warn_unrecorded(
                            "complete abandoned-session retirement request",
                            self.complete_session_retirement_request(request_generation),
                        );
                    }
                    Some(report)
                }
                Err(error) => {
                    crate::warn_unrecorded(
                        "run bounded automatic checkout cleanup",
                        Err::<(), _>(error),
                    );
                    None
                }
            }
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
        self.gc_apply_with(confirm, budget_ms, false, None)
    }

    fn gc_apply_with(
        &mut self,
        confirm: &str,
        budget_ms: Option<u64>,
        include_active_gate_cache: bool,
        planning_deadline: Option<Instant>,
    ) -> Result<GcApplyReport, BrokerOpError> {
        if confirm.len() != 64 || !confirm.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BrokerOpError::GcConfirmationNotSha256);
        }
        let main_root = self.main_root().to_path_buf();
        let _lock = GcLock::acquire(&main_root)?;
        let journal_path = main_root.join(".aethyme/gc-journal.json");
        let mut journal = match load_journal(&journal_path)? {
            Some(journal) => {
                if !gc_journal_schema_is_resumable(journal.schema_version) {
                    let reason = if journal.schema_version == 2 {
                        format!(
                            "schema 2 is no longer resumable (supported resumable schemas: {} and {}); abandon this journal with `aethyme broker gc abandon --confirm {}` to archive it without removing retained data, then review a fresh `aethyme broker gc plan`",
                            GC_LEGACY_PLAN_SCHEMA_VERSION, GC_PLAN_SCHEMA_VERSION, journal.digest
                        )
                    } else {
                        format!(
                            "schema {} is unsupported (expected {} or {}); this unknown journal version cannot be safely resumed or abandoned",
                            journal.schema_version,
                            GC_LEGACY_PLAN_SCHEMA_VERSION,
                            GC_PLAN_SCHEMA_VERSION
                        )
                    };
                    return Err(BrokerOpError::InvalidGcJournal { reason });
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
                let plan = match planning_deadline {
                    Some(deadline) => self.gc_plan_scanned_until(
                        crate::SizeScan::Measure,
                        include_active_gate_cache,
                        Some(deadline),
                    )?,
                    None => {
                        self.gc_plan_scanned(crate::SizeScan::Measure, include_active_gate_cache)?
                    }
                };
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
        let age_reference_ms = gc_journal_age_reference_ms(&journal)?;
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
            let count = journal
                .remaining_rows
                .len()
                .min(crate::error::GC_ROW_DELETE_BATCH_SIZE);
            let batch = journal.remaining_rows[..count].to_vec();
            self.store().delete_gc_rows(
                &batch,
                cutoff(age_reference_ms, journal.policy.terminal_events_days),
                cutoff(age_reference_ms, journal.policy.gate_results_days),
                cutoff(age_reference_ms, journal.policy.terminal_merge_queue_days),
            )?;
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
                            cutoff(age_reference_ms, journal.policy.command_metrics_days),
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
            // A fresh live-owner observation takes precedence over the old
            // content verdict, so an adopted checkout is retained and does
            // not pin the remaining cleanup journal.
            {
                // Re-check even if the path was absent in the reviewed plan:
                // another session may have recreated and adopted it since.
                match self
                    .refuse_live_checkout(candidate.session_id, Path::new(&candidate.worktree_path))
                {
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
            // The operator confirmed this artifact at plan time. On Unix,
            // serialize with gate admission and hold its per-checkout use
            // exclusion through every unlink; confirmation alone cannot
            // authorize removal after a gate has started using the checkout.
            // Keep the established explicit-cleanup behavior on platforms
            // where the artifact-use protocol is unavailable.
            #[cfg(unix)]
            let (_lifecycle_claim, _artifact_use_reclaimer) = {
                let lifecycle_claim = match self.try_claim_worktree_lifecycle() {
                    Ok(Some(claim)) => claim,
                    Ok(None) => {
                        failures.push(format!(
                            "{}: checkout lifecycle is contended; artifact was retained",
                            candidate.relative_dir
                        ));
                        break;
                    }
                    Err(error) => {
                        failures.push(format!(
                            "{}: checkout lifecycle is unavailable ({error}); artifact was retained",
                            candidate.relative_dir
                        ));
                        break;
                    }
                };
                let current_session = self.store().session(candidate.session_id)?;
                if !current_session.status.is_closed()
                    || current_session.cleanup_state != crate::SessionCleanupState::Closed
                    || current_session.worktree_path != candidate.worktree_path
                {
                    failures.push(format!(
                        "{}: checkout session changed; review a new GC plan",
                        candidate.relative_dir
                    ));
                    journal.remaining_artifacts.remove(0);
                    write_journal(&journal_path, &journal)?;
                    continue;
                }
                let live_checkout = match deadline {
                    Some(deadline) => {
                        self.refuse_live_checkout_until(candidate.session_id, &root, deadline)
                    }
                    None => self.refuse_live_checkout(candidate.session_id, &root),
                };
                match live_checkout {
                    Ok(()) => {}
                    Err(BrokerOpError::WorktreeInUseByLiveSession { .. }) => {
                        failures.push(format!(
                            "{}: a live checkout now uses this worktree; review a new GC plan",
                            candidate.relative_dir
                        ));
                        journal.remaining_artifacts.remove(0);
                        write_journal(&journal_path, &journal)?;
                        continue;
                    }
                    Err(error) => {
                        failures.push(format!(
                            "{}: live checkout state is unavailable ({error}); artifact was retained",
                            candidate.relative_dir
                        ));
                        break;
                    }
                }
                let artifact_use_reclaimer =
                    match crate::file_lock::artifact_use::try_artifact_use_reclaimer(
                        &main_root, &root,
                    ) {
                        Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(
                            reclaimer,
                        )) => reclaimer,
                        Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(
                            block,
                        )) => {
                            let reason = match block {
                                crate::file_lock::artifact_use::ArtifactUseReclaimBlock::Contended => {
                                    "artifact-use claim is contended"
                                }
                                crate::file_lock::artifact_use::ArtifactUseReclaimBlock::UnknownProducer {
                                    ..
                                } => "an active or unknown gate use was found",
                                crate::file_lock::artifact_use::ArtifactUseReclaimBlock::ProtocolUnavailable => {
                                    "artifact-use protocol is unavailable"
                                }
                            };
                            failures.push(format!(
                                "{}: {reason}; artifact was retained",
                                candidate.relative_dir
                            ));
                            break;
                        }
                        Err(error) => {
                            failures.push(format!(
                                "{}: artifact-use inventory failed ({error}); artifact was retained",
                                candidate.relative_dir
                            ));
                            break;
                        }
                    };
                (lifecycle_claim, artifact_use_reclaimer)
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
            let gate_storage = self.storage_context().gate_storage_locations();
            let location = crate::gate_cache_gc::location_in(&main_root, &gate_storage);
            let reclaim = match location.as_ref() {
                Some(location) => {
                    crate::gate_cache_gc::reclaim_in(&main_root, location, &candidate, deadline)
                }
                None => {
                    Err("this repository's captured gate cache location cannot be resolved".into())
                }
            };
            match reclaim {
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

        // Visit each candidate at most once per apply. A temporarily unreadable
        // tree stays durable at the tail without starving healthy roots.
        let orphan_visits = journal.remaining_orphans.len();
        for _ in 0..orphan_visits {
            if journal.remaining_orphans.is_empty() || check_deadline(deadline) {
                break;
            }
            let candidate = journal.remaining_orphans[0].clone();
            let root = PathBuf::from(&candidate.worktree_root);
            let Some(fingerprint) = candidate.ownership_fingerprint.as_deref() else {
                failures.push(format!("{}: legacy orphan authorization revoked; no deletion performed; review a fresh GC plan", candidate.worktree_root));
                journal.remaining_orphans.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            };
            let database = self.host_operation_database_path()?;
            // Keep exclusion through every unlink and journal update. A busy
            // registry or unavailable filesystem leaves durable work pending.
            let mut removal_claim = match crate::worktree_registry::claim_for_removal(
                &database,
                &root,
                fingerprint,
            ) {
                Ok(claim) => claim,
                Err(crate::worktree_registry::ClaimError::Revoked(reason)) => {
                    failures.push(format!("{}: orphan authorization revoked ({reason}); no deletion performed; review a fresh GC plan", candidate.worktree_root));
                    journal.remaining_orphans.remove(0);
                    write_journal(&journal_path, &journal)?;
                    continue;
                }
                Err(crate::worktree_registry::ClaimError::HistoryLost(reason)) => {
                    failures.push(format!("{}: orphan authorization revoked ({reason}); no deletion performed. Saved history cannot safely authorize reclamation; a fresh plan may omit this root. Independently recover ownership before requesting its removal", candidate.worktree_root));
                    journal.remaining_orphans.remove(0);
                    write_journal(&journal_path, &journal)?;
                    continue;
                }
                Err(crate::worktree_registry::ClaimError::Unavailable(error)) => {
                    failures.push(format!(
                        "{}: orphan removal temporarily unavailable ({error}); retained in journal",
                        candidate.worktree_root
                    ));
                    journal.remaining_orphans.rotate_left(1);
                    write_journal(&journal_path, &journal)?;
                    continue;
                }
            };
            if removal_claim.already_absent {
                journal.remaining_orphans.remove(0);
                write_journal(&journal_path, &journal)?;
                continue;
            }
            // The root marker is the orphan evidence, so it goes last for the
            // same reason a build cache's witness does.
            match removal_claim.remove(deadline) {
                Ok(TreeRemoval::Complete) => {}
                Ok(TreeRemoval::Interrupted) => break,
                Err(error) => {
                    failures.push(format!(
                        "{}: orphan deletion unavailable ({error}); retained in journal",
                        candidate.worktree_root
                    ));
                    journal.remaining_orphans.rotate_left(1);
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
                    .ok_or_else(|| {
                        "no longer meets recovery retention or landing requirements".to_string()
                    })?;
                Ok(archive)
            });
            let archive = match evidence {
                Ok(archive) => archive,
                Err(reason) => {
                    failures.push(format!(
                        "{}: {reason}; review a new GC plan",
                        candidate.path
                    ));
                    journal.remaining_recovery_archives.remove(0);
                    write_journal(&journal_path, &journal)?;
                    continue;
                }
            };
            // The manifest is the ownership evidence, so it goes last.
            let removal = {
                #[cfg(test)]
                {
                    let mut before_mutation = || tests::before_gc_artifact_removal_mutation(&dir);
                    remove_condemned_tree_with_progress_and_guard(
                        &dir,
                        Some(crate::cleanup_resolve::MANIFEST_FILE),
                        deadline,
                        &mut before_mutation,
                    )
                    .map(|(outcome, _entries_reclaimed)| outcome)
                    .map_err(|error| error.source)
                }
                #[cfg(not(test))]
                {
                    remove_condemned_tree(
                        &dir,
                        Some(crate::cleanup_resolve::MANIFEST_FILE),
                        deadline,
                    )
                }
            };
            match removal {
                Ok(TreeRemoval::Complete) => {}
                Ok(TreeRemoval::Interrupted) => break,
                Err(error) => {
                    failures.push(format!("{}: {error}", candidate.path));
                    journal.remaining_recovery_archives.remove(0);
                    write_journal(&journal_path, &journal)?;
                    continue;
                }
            }
            match std::fs::symlink_metadata(&dir) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match self.release_recovery_archive_prerequisite_refs_after_removal(&archive) {
                        Ok(()) => {}
                        Err(error) => {
                            failures.push(format!(
                                "{}: archive was removed but its prerequisite refs could not be released ({error}); refs were retained for safety",
                                candidate.path
                            ));
                        }
                    }
                }
                Ok(_) => {
                    failures.push(format!(
                        "{}: archive removal could not be verified; prerequisite refs were retained",
                        candidate.path
                    ));
                    break;
                }
                Err(error) => {
                    failures.push(format!(
                        "{}: could not verify archive removal ({error}); prerequisite refs were retained",
                        candidate.path
                    ));
                    break;
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
    use std::cell::RefCell;

    enum ArtifactSweepTestMutation {
        InterruptAfterFirstUnlink {
            path: PathBuf,
            attempts: usize,
        },
        RewriteFileBeforeRemoval {
            path: PathBuf,
            file: PathBuf,
            contents: String,
            fired: bool,
        },
        #[cfg(unix)]
        ReplaceDirectoryWithSymlink {
            trigger_path: PathBuf,
            replaced_path: PathBuf,
            moved_to: PathBuf,
            referent: PathBuf,
            fired: bool,
        },
    }

    std::thread_local! {
        // Observe the real launch boundary without bypassing test suppression
        // or spawning detached processes in the test runner.
        pub(super) static ARTIFACT_WORKER_ADMISSION_ATTEMPTS: RefCell<Vec<bool>> =
            const { RefCell::new(Vec::new()) };
        static ARTIFACT_SWEEP_TEST_MUTATION_INTERRUPT: RefCell<Option<ArtifactSweepTestMutation>> =
            const { RefCell::new(None) };
    }

    struct ArtifactSweepTestMutationInterrupt {
        previous: Option<ArtifactSweepTestMutation>,
    }

    impl ArtifactSweepTestMutationInterrupt {
        fn after_first_unlink(path: &Path) -> Self {
            let previous = ARTIFACT_SWEEP_TEST_MUTATION_INTERRUPT.with(|state| {
                state.replace(Some(ArtifactSweepTestMutation::InterruptAfterFirstUnlink {
                    path: path.to_path_buf(),
                    attempts: 0,
                }))
            });
            Self { previous }
        }

        fn rewrite_file_before_removal(path: &Path, file: &Path, contents: &str) -> Self {
            let previous = ARTIFACT_SWEEP_TEST_MUTATION_INTERRUPT.with(|state| {
                state.replace(Some(ArtifactSweepTestMutation::RewriteFileBeforeRemoval {
                    path: path.to_path_buf(),
                    file: file.to_path_buf(),
                    contents: contents.to_owned(),
                    fired: false,
                }))
            });
            Self { previous }
        }

        #[cfg(unix)]
        fn replace_directory_with_symlink(
            trigger_path: &Path,
            replaced_path: &Path,
            moved_to: &Path,
            referent: &Path,
        ) -> Self {
            let previous = ARTIFACT_SWEEP_TEST_MUTATION_INTERRUPT.with(|state| {
                state.replace(Some(
                    ArtifactSweepTestMutation::ReplaceDirectoryWithSymlink {
                        trigger_path: trigger_path.to_path_buf(),
                        replaced_path: replaced_path.to_path_buf(),
                        moved_to: moved_to.to_path_buf(),
                        referent: referent.to_path_buf(),
                        fired: false,
                    },
                ))
            });
            Self { previous }
        }
    }

    impl Drop for ArtifactSweepTestMutationInterrupt {
        fn drop(&mut self) {
            let previous = self.previous.take();
            ARTIFACT_SWEEP_TEST_MUTATION_INTERRUPT.with(|state| {
                state.replace(previous);
            });
        }
    }

    pub(super) fn before_gc_artifact_removal_mutation(path: &Path) -> std::io::Result<()> {
        ARTIFACT_SWEEP_TEST_MUTATION_INTERRUPT.with(|state| {
            let mut state = state.borrow_mut();
            let Some(mutation) = state.as_mut() else {
                return Ok(());
            };
            match mutation {
                ArtifactSweepTestMutation::InterruptAfterFirstUnlink {
                    path: target,
                    attempts,
                } if target.as_path() == path => {
                    *attempts += 1;
                    if *attempts >= 2 {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "test hook stopped after the first cache unlink",
                        ))
                    } else {
                        Ok(())
                    }
                }
                ArtifactSweepTestMutation::RewriteFileBeforeRemoval {
                    path: target,
                    file,
                    contents,
                    fired,
                } if target.as_path() == path && !*fired => {
                    *fired = true;
                    std::fs::write(file, contents.as_bytes())
                }
                #[cfg(unix)]
                ArtifactSweepTestMutation::ReplaceDirectoryWithSymlink {
                    trigger_path: target,
                    replaced_path,
                    moved_to,
                    referent,
                    fired,
                } if target.as_path() == path && !*fired => {
                    *fired = true;
                    std::fs::rename(replaced_path.as_path(), moved_to.as_path())?;
                    std::os::unix::fs::symlink(referent.as_path(), replaced_path.as_path())
                }
                _ => Ok(()),
            }
        })
    }

    #[test]
    fn interrupted_v6_archive_removal_keeps_its_prerequisite_refs() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
        git(&root, &["add", "README.md", ".gitignore"]);
        git(&root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nrecovery_archive_days = 1\n",
        )
        .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let session = broker
            .start_worktree("archive prerequisite fixture", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("unique.txt"), "archive prerequisite\n").unwrap();
        git(&worktree, &["add", "unique.txt"]);
        git(&worktree, &["commit", "-qm", "unique archive work"]);
        broker.close(session.id).unwrap();
        let resolve_plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        let archived = broker
            .cleanup_resolve_retirement_apply(session.id, &resolve_plan.digest, false)
            .unwrap();
        assert!(archived.worktree_removed);
        let archive = archived.archive;
        let initial_archive = broker.owned_recovery_archive(&archive).unwrap();
        assert!(!initial_archive.prerequisite_refs.is_empty());
        let pin_refs = initial_archive.prerequisite_refs.clone();

        let manifest_path = archive.join(crate::cleanup_resolve::MANIFEST_FILE);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["schema_version"], 6, "{manifest}");
        let created_at_ms = manifest["created_at_ms"].as_i64().unwrap();
        let aged_at_ms = created_at_ms - 16 * MILLIS_PER_DAY;
        let retirement_path = archive.join("retirement.json");
        let mut retirement: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&retirement_path).unwrap()).unwrap();
        assert_eq!(retirement["archive_created_at_ms"], created_at_ms);
        retirement["archive_created_at_ms"] = serde_json::json!(aged_at_ms);
        let retirement_bytes = serde_json::to_vec(&retirement).unwrap();
        manifest["retirement_record_sha256"] =
            serde_json::json!(format!("{:x}", Sha256::digest(&retirement_bytes)));
        manifest["created_at_ms"] = serde_json::json!(aged_at_ms);
        std::fs::write(&retirement_path, retirement_bytes).unwrap();
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let reviewed = broker.gc_plan().unwrap();
        assert_eq!(reviewed.recovery_archives.len(), 1, "{reviewed:?}");
        for prerequisite in &pin_refs {
            let output = Command::new("git")
                .args(["rev-parse", "--verify", &prerequisite.ref_name])
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", prerequisite.ref_name);
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                prerequisite.object_id
            );
        }

        let _interrupt_after_first_unlink =
            ArtifactSweepTestMutationInterrupt::after_first_unlink(&archive);
        let applied = broker.gc_apply(&reviewed.digest).unwrap();

        assert!(
            archive.exists(),
            "interrupted deletion leaves a partial archive"
        );
        assert!(
            applied.recovery_archives_removed.is_empty(),
            "partial deletion is not reported as complete: {applied:?}"
        );
        assert!(
            applied.failures.iter().any(|failure| {
                failure.contains("test hook stopped after the first cache unlink")
            })
        );
        for prerequisite in &pin_refs {
            let output = Command::new("git")
                .args(["rev-parse", "--verify", &prerequisite.ref_name])
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", prerequisite.ref_name);
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                prerequisite.object_id
            );
        }
    }

    fn test_worker_retry_pending(broker: &mut Broker, now: i64) -> Result<bool, BrokerOpError> {
        let namespace = broker.ensure_artifact_sweep_namespace()?;
        worker_retry_pending(broker.store(), &namespace, now)
    }

    #[test]
    fn compact_batch_visits_multiple_candidates_without_another_worker() {
        let mut seen = Vec::new();
        let complete = run_compact_checkout_candidates(
            &[10, 11, 12],
            || AUTO_COMPACT_CHECKOUT_WORKER_RESERVE,
            |id| {
                seen.push(*id);
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert!(complete);
        assert_eq!(seen, vec![10, 11, 12]);
    }

    #[test]
    fn compact_batch_preserves_archive_reserve_before_each_attempt() {
        let remaining = std::cell::Cell::new(AUTO_COMPACT_CHECKOUT_WORKER_RESERVE);
        let mut seen = Vec::new();
        let complete = run_compact_checkout_candidates(
            &[10, 11, 12],
            || remaining.get(),
            |id| {
                seen.push(*id);
                remaining
                    .set(crate::auto_cleanup::AUTO_COMPACTION_MIN_BUDGET - Duration::from_nanos(1));
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert!(!complete);
        assert_eq!(seen, vec![10]);
        let complete = run_compact_checkout_candidates(
            &[11],
            || Duration::ZERO,
            |_| -> Result<(), ()> { panic!("expired reserve must not dispatch an attempt") },
        )
        .unwrap();
        assert!(!complete);
    }

    #[test]
    fn compact_batch_stops_on_store_error_and_bounds_candidate_count() {
        let mut seen = Vec::new();
        let error = run_compact_checkout_candidates(
            &[10, 11],
            || AUTO_COMPACT_CHECKOUT_WORKER_RESERVE,
            |id| {
                seen.push(*id);
                Err("database unavailable")
            },
        );
        assert_eq!(error, Err("database unavailable"));
        assert_eq!(seen, vec![10]);
        let candidates = (0..AUTO_COMPACT_CHECKOUT_BATCH_SIZE + 3).collect::<Vec<_>>();
        let mut seen = Vec::new();
        run_compact_checkout_candidates(
            &candidates,
            || AUTO_COMPACT_CHECKOUT_WORKER_RESERVE,
            |id| {
                seen.push(*id);
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert_eq!(seen, candidates[..AUTO_COMPACT_CHECKOUT_BATCH_SIZE]);
    }

    #[test]
    fn compact_worker_has_separate_reserve_and_due_admission() {
        assert_eq!(
            artifact_worker_cache_lane_duration(false, false, false),
            ARTIFACT_SWEEP_WORKER_MAX_DURATION.saturating_sub(SESSION_RETIREMENT_WORKER_RESERVE)
        );
        assert_eq!(
            artifact_worker_cache_lane_duration(true, false, false),
            ARTIFACT_SWEEP_WORKER_MAX_DURATION.saturating_sub(
                AUTO_CHECKOUT_CLEANUP_WORKER_RESERVE
                    .saturating_add(SESSION_RETIREMENT_WORKER_RESERVE)
            )
        );
        assert_eq!(
            artifact_worker_cache_lane_duration(false, true, false),
            ARTIFACT_SWEEP_WORKER_MAX_DURATION.saturating_sub(
                AUTO_COMPACT_CHECKOUT_WORKER_RESERVE
                    .saturating_add(SESSION_RETIREMENT_WORKER_RESERVE)
            )
        );
        assert_eq!(
            artifact_worker_cache_lane_duration(true, true, false),
            ARTIFACT_SWEEP_WORKER_MAX_DURATION.saturating_sub(
                AUTO_CHECKOUT_CLEANUP_WORKER_RESERVE
                    .saturating_add(AUTO_COMPACT_CHECKOUT_WORKER_RESERVE)
                    .saturating_add(SESSION_RETIREMENT_WORKER_RESERVE)
            )
        );
        assert_eq!(
            artifact_worker_cache_lane_duration(false, false, true),
            ARTIFACT_SWEEP_WORKER_MAX_DURATION
                .saturating_sub(SESSION_RETIREMENT_WORKER_RESERVE.saturating_mul(2))
        );
        assert_eq!(
            artifact_worker_cache_lane_duration(true, true, true),
            ARTIFACT_SWEEP_WORKER_MAX_DURATION.saturating_sub(
                AUTO_CHECKOUT_CLEANUP_WORKER_RESERVE
                    .saturating_add(SESSION_RETIREMENT_WORKER_RESERVE)
                    .saturating_add(SESSION_RETIREMENT_WORKER_RESERVE)
                    .saturating_add(AUTO_COMPACT_CHECKOUT_WORKER_RESERVE)
            )
        );
        // Consume the full cache grant plus ordinary handoff time, then feed
        // that remaining budget into the compact-lane admission decision.
        let normal_handoff_overhead = Duration::from_secs(5);
        let compact_only_cache_budget = artifact_worker_cache_lane_duration(false, true, false);
        let compact_only_remaining = ARTIFACT_SWEEP_WORKER_MAX_DURATION
            .saturating_sub(compact_only_cache_budget.saturating_add(normal_handoff_overhead));
        assert_eq!(compact_only_cache_budget, Duration::from_secs(180));
        assert_eq!(compact_only_remaining, Duration::from_secs(115));
        assert_eq!(
            compact_checkout_worker_pass_budget(compact_only_remaining, false),
            Some(AUTO_COMPACT_CHECKOUT_WORKER_RESERVE)
        );
        assert_eq!(
            compact_checkout_worker_pass_budget(
                AUTO_COMPACT_CHECKOUT_WORKER_ADMISSION_MINIMUM - Duration::from_nanos(1),
                false,
            ),
            None
        );
        assert_eq!(
            compact_checkout_worker_pass_budget(AUTO_COMPACT_CHECKOUT_WORKER_RESERVE, false),
            Some(AUTO_COMPACT_CHECKOUT_WORKER_RESERVE)
        );

        let both_due_cache_budget = artifact_worker_cache_lane_duration(true, true, false);
        let both_due_remaining = ARTIFACT_SWEEP_WORKER_MAX_DURATION
            .saturating_sub(both_due_cache_budget.saturating_add(normal_handoff_overhead));
        let both_due_compact_budget =
            compact_checkout_worker_pass_budget(both_due_remaining, true).unwrap();
        assert_eq!(both_due_cache_budget, Duration::from_secs(120));
        assert_eq!(both_due_compact_budget, Duration::from_secs(90));
        assert_eq!(
            both_due_remaining.saturating_sub(both_due_compact_budget),
            Duration::from_secs(85)
        );
        assert_eq!(
            compact_checkout_worker_pass_budget(
                AUTO_CHECKOUT_CLEANUP_WORKER_RESERVE
                    .saturating_add(AUTO_COMPACT_CHECKOUT_WORKER_ADMISSION_MINIMUM)
                    .saturating_sub(Duration::from_nanos(1)),
                true,
            ),
            None
        );
        assert!(!compact_checkout_pending_is_due(
            true,
            Some(101),
            Some(200),
            100
        ));
        assert!(compact_checkout_pending_is_due(
            true,
            Some(99),
            Some(200),
            100
        ));
        assert!(compact_checkout_pending_is_due(false, None, Some(100), 100));
        assert!(!compact_checkout_pending_is_due(
            false,
            None,
            Some(101),
            100
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn compact_missing_checkout_reports_reason_and_keeps_request_pending() {
        let (_temporary, _storage, mut broker, _root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, _) =
            closed_artifact_cache(&mut broker, "missing compact checkout");
        broker
            .store()
            .request_closed_spawned_checkout_compactions(&[session_id], 0, now_ms())
            .unwrap();
        std::fs::remove_dir_all(&worktree).unwrap();

        broker
            .auto_compact_checkout_candidate(session_id, Instant::now() + Duration::from_secs(30))
            .unwrap();

        let expected_status = format!(
            "compact_checkout_cohort_unavailable session={session_id} reason=checkout_missing"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_WORKER_STATUS_KEY)
                .unwrap()
                .as_deref(),
            Some(expected_status.as_str())
        );
        assert!(
            broker
                .store()
                .next_pending_checkout_compaction_at()
                .unwrap()
                .is_some(),
            "a missing closed checkout must remain pending until safe terminal reconciliation"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn compact_pass_removes_two_checkouts_with_fresh_cohorts_and_valid_archives() {
        let (_temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (first, first_path, _) = closed_artifact_cache(&mut broker, "compact batch first");
        let (second, second_path, _) = closed_artifact_cache(&mut broker, "compact batch second");
        for (path, payload) in [
            (&first_path, "first unique work"),
            (&second_path, "second unique work"),
        ] {
            std::fs::write(path.join("unique-commit.txt"), payload).unwrap();
            git(path, &["add", "unique-commit.txt"]);
            git(path, &["commit", "-qm", "preserved batch work"]);
            std::fs::write(path.join("recover-me.txt"), payload).unwrap();
        }
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nauto_remove = true\nstrict = false\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\ncompact_closed_worktrees = true\nclosed_worktree_grace_hours = 360\nauto_cleanup_worktrees_on_finish = false\nartifact_sweep_budget_ms = 0\n",
        ).unwrap();
        assert_eq!(
            broker
                .store()
                .request_closed_spawned_checkout_compactions(&[first, second], 360, now_ms(),)
                .unwrap(),
            2
        );
        assert_eq!(
            broker
                .store()
                .due_checkout_cleanup_compactions(now_ms(), 8)
                .unwrap(),
            vec![first, second]
        );

        broker
            .auto_compact_checkout_recovery_pass(
                Instant::now() + AUTO_COMPACT_CHECKOUT_WORKER_RESERVE,
            )
            .unwrap();

        assert!(
            !first_path.exists(),
            "first native checkout must be retired"
        );
        assert!(
            !second_path.exists(),
            "second proof must refresh after the first removal dirties the alias index"
        );
        assert!(
            broker
                .store()
                .due_checkout_cleanup_compactions(now_ms(), 8)
                .unwrap()
                .is_empty()
        );
        let archives = broker.recovery_archives().unwrap();
        assert!(archives.unowned.is_empty(), "{:?}", archives.unowned);
        assert_eq!(archives.owned.len(), 2);
        for id in [first, second] {
            let archive = archives.owned.iter().find(|a| a.session_id == id).unwrap();
            assert!(
                archive.holds_uncommitted,
                "untracked work must remain recoverable"
            );
            assert!(
                !archive.prerequisite_refs.is_empty(),
                "incremental archive roots must remain pinned"
            );
            assert!(archive.minimum_archive_retention_ms.unwrap() >= 15 * 24 * 60 * 60 * 1_000);
            for pin in &archive.prerequisite_refs {
                let output = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(["rev-parse", "--verify", &pin.ref_name])
                    .output()
                    .unwrap();
                assert!(output.status.success());
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    pin.object_id
                );
            }
        }
    }

    #[test]
    fn foreground_sweep_reaches_worker_admission_for_compact_only_backlog() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        let config = root.join(crate::retention::BROKER_CONFIG_RELPATH);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "[retention]\ncompact_closed_worktrees = true\n").unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        assert!(broker.reconcile_compact_checkout_backfill().unwrap());
        assert!(broker.compact_checkout_work_pending().unwrap());
        assert!(!broker.auto_checkout_cleanup_recovery_requested().unwrap());

        assert!(broker.compact_checkout_work_due(now_ms()).unwrap());
        assert!(!broker.artifact_sweep_backlog_pending().unwrap());
        ARTIFACT_WORKER_ADMISSION_ATTEMPTS.with(|attempts| attempts.borrow_mut().clear());
        let report = broker.gc_sweep_artifacts().unwrap();

        assert!(report.complete, "{report:?}");
        assert!(!broker.artifact_sweep_backlog_pending().unwrap());
        assert!(!broker.auto_checkout_cleanup_recovery_requested().unwrap());
        assert!(broker.compact_checkout_work_pending().unwrap());
        assert_eq!(
            ARTIFACT_WORKER_ADMISSION_ATTEMPTS.with(|attempts| attempts.borrow().clone()),
            vec![false],
            "a compact-only backlog must reach the real worker admission boundary"
        );
    }

    #[test]
    fn explicit_sweep_reports_incomplete_until_gc_lock_is_released() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "7")
            .unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "3")
            .unwrap();

        let held = GcLock::acquire(root).unwrap();
        let report = broker.gc_sweep_artifacts().unwrap();
        assert!(
            !report.complete,
            "lock contention skipped inspection: {report:?}"
        );
        assert_eq!(report.directories_reclaimed, 0);
        assert!(report.auto_cleanup.is_none());
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("3"),
            "a skipped pass must not acknowledge pending recovery"
        );
        drop(held);

        let retry = broker.gc_sweep_artifacts().unwrap();
        assert!(
            retry.complete,
            "the unlocked namespace can be inspected: {retry:?}"
        );
    }

    #[test]
    fn compact_stock_backfill_is_queued_on_opt_in_and_resumes_its_generation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        let config = root.join(crate::retention::BROKER_CONFIG_RELPATH);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "[retention]\ncompact_closed_worktrees = false\n").unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        assert!(!broker.reconcile_compact_checkout_backfill().unwrap());
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_REQUESTED_KEY)
                .unwrap(),
            None,
            "disabled policy does not queue a historical checkout scan"
        );

        std::fs::write(&config, "[retention]\ncompact_closed_worktrees = true\n").unwrap();
        assert!(broker.reconcile_compact_checkout_backfill().unwrap());
        let first_generation = broker
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_REQUESTED_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some("0")
        );
        broker
            .artifact_sweep_meta_set(AUTO_COMPACT_BACKFILL_CURSOR_KEY, "24")
            .unwrap();
        broker
            .artifact_sweep_meta_set(AUTO_COMPACT_BACKFILL_HIGH_WATER_KEY, "900")
            .unwrap();
        assert!(broker.reconcile_compact_checkout_backfill().unwrap());
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some(first_generation.as_str()),
            "an incomplete opt-in backfill resumes its existing generation"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some("24")
        );

        std::fs::write(&config, "[retention]\ncompact_closed_worktrees = false\n").unwrap();
        assert!(!broker.reconcile_compact_checkout_backfill().unwrap());
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some(first_generation.as_str()),
            "opt-out pauses rather than erases pending work"
        );
        std::fs::write(&config, "[retention]\ncompact_closed_worktrees = true\n").unwrap();
        assert!(broker.reconcile_compact_checkout_backfill().unwrap());
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some("24"),
            "re-enabling a pending generation resumes from its durable cursor"
        );

        broker
            .artifact_sweep_meta_set(AUTO_COMPACT_BACKFILL_COMPLETED_KEY, &first_generation)
            .unwrap();
        std::fs::write(&config, "[retention]\ncompact_closed_worktrees = false\n").unwrap();
        assert!(!broker.reconcile_compact_checkout_backfill().unwrap());
        std::fs::write(&config, "[retention]\ncompact_closed_worktrees = true\n").unwrap();
        assert!(broker.reconcile_compact_checkout_backfill().unwrap());
        let next_generation = broker
            .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_REQUESTED_KEY)
            .unwrap()
            .unwrap();
        assert!(next_generation.parse::<i64>().unwrap() > first_generation.parse::<i64>().unwrap());
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some("0"),
            "a completed backfill is restarted after a later opt-in edge"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_COMPACT_BACKFILL_HIGH_WATER_KEY)
                .unwrap()
                .as_deref(),
            Some("")
        );
    }

    #[test]
    fn artifact_sweep_state_is_shared_by_linked_worktrees_but_isolated_by_storage_context() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nartifact_sweep_budget_ms = 0\n",
        )
        .unwrap();
        let linked = temporary.path().join("linked-worktree");
        git(
            &root,
            &[
                "worktree",
                "add",
                "--detach",
                linked.to_str().unwrap(),
                "HEAD",
            ],
        );

        let storage_a = crate::test_storage::TestStorage::new();
        let mut root_broker = storage_a.open(&root).unwrap();
        let root_namespace = root_broker.ensure_artifact_sweep_namespace().unwrap();
        let interrupted_a = root.join("target").to_string_lossy().into_owned();
        root_broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_CURSOR_KEY, "41")
            .unwrap();
        root_broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "9")
            .unwrap();
        root_broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "4")
            .unwrap();
        root_broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_INTERRUPTED_KEY, &interrupted_a)
            .unwrap();
        let captured_cache_root = root_broker
            .storage_context()
            .preparation_cache_root()
            .unwrap()
            .unwrap();
        assert!(
            root_broker
                .artifact_sweep_budget_roots(ArtifactSweepScope::AutomaticContinuation)
                .contains(&captured_cache_root)
        );
        drop(root_broker);

        let mut linked_broker = storage_a.open(&linked).unwrap();
        let expected_main_root = root.canonicalize().unwrap();
        assert_eq!(linked_broker.main_root(), expected_main_root.as_path());
        assert_eq!(
            linked_broker.ensure_artifact_sweep_namespace().unwrap(),
            root_namespace,
            "a linked checkout in the same repository and storage context reuses its namespace"
        );
        assert_eq!(
            linked_broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some("41")
        );
        assert_eq!(
            linked_broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("9")
        );
        assert_eq!(
            linked_broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("4")
        );
        assert_eq!(
            linked_broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                .unwrap()
                .as_deref(),
            Some(interrupted_a.as_str())
        );
        let registered: Vec<String> = serde_json::from_str(
            &linked_broker
                .store()
                .meta_get("gc.artifact_sweep.v2.context_registry")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            registered,
            vec![root_namespace.clone()],
            "linked-worktree opens do not consume additional namespace slots"
        );
        drop(linked_broker);

        let storage_b = crate::test_storage::TestStorage::new();
        let mut broker_b = storage_b.open(&root).unwrap();
        let namespace_b = broker_b.ensure_artifact_sweep_namespace().unwrap();
        assert_ne!(root_namespace, namespace_b);
        broker_b
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_CURSOR_KEY, "7")
            .unwrap();
        broker_b
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "12")
            .unwrap();
        broker_b
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "12")
            .unwrap();
        broker_b
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_INTERRUPTED_KEY, "")
            .unwrap();
        assert_eq!(
            broker_b
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some("7")
        );
        assert_eq!(
            broker_b
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("12")
        );
        assert_eq!(
            broker_b
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("12")
        );
        assert_eq!(
            broker_b
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                .unwrap()
                .as_deref(),
            Some("")
        );
        drop(broker_b);

        let mut reopened_a = storage_a.open(&root).unwrap();
        assert_eq!(
            reopened_a.ensure_artifact_sweep_namespace().unwrap(),
            root_namespace
        );
        assert_eq!(
            reopened_a
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some("41"),
            "a second effective storage context cannot overwrite the first context's state"
        );
        assert_eq!(
            reopened_a
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("9"),
            "another context cannot overwrite an outstanding recovery generation"
        );
        assert_eq!(
            reopened_a
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("4")
        );
        assert_eq!(
            reopened_a
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                .unwrap()
                .as_deref(),
            Some(interrupted_a.as_str()),
            "another context cannot clear interrupted-path debt"
        );
    }

    #[test]
    fn legacy_artifact_sweep_state_is_reported_without_rewriting_raw_values() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nartifact_sweep_budget_ms = 0\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        let clear_values = [
            (ARTIFACT_SWEEP_STAMP_KEY, "123"),
            (ARTIFACT_SWEEP_CURSOR_KEY, "0"),
            (ARTIFACT_SWEEP_PROGRESS_KEY, "0"),
            (ARTIFACT_SWEEP_INTERRUPTED_KEY, ""),
            (ARTIFACT_SWEEP_BLOCKED_PATHS_KEY, "[]"),
            (ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "5"),
            (ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "5"),
            (ARTIFACT_SWEEP_RETRY_AFTER_KEY, "0"),
            (
                ARTIFACT_SWEEP_WORKER_STATUS_KEY,
                "complete passes=1 entries=0",
            ),
        ];
        for (key, value) in clear_values {
            broker.store().meta_set(key, value).unwrap();
        }

        assert_eq!(
            broker.legacy_artifact_sweep_state(),
            ArtifactSweepLegacyUnscopedState::Clear,
            "a positive old stamp or completed worker status alone is not pending work"
        );
        for (key, value) in clear_values {
            assert_eq!(
                broker.store().meta_get(key).unwrap().as_deref(),
                Some(value)
            );
        }

        let oversized_status = "x".repeat(ARTIFACT_SWEEP_LEGACY_SCALAR_MAX_BYTES + 1);
        broker
            .store()
            .meta_set(ARTIFACT_SWEEP_WORKER_STATUS_KEY, &oversized_status)
            .unwrap();
        assert_eq!(
            broker.legacy_artifact_sweep_state(),
            ArtifactSweepLegacyUnscopedState::UnknownPending,
            "oversized legacy text is not loaded for status inspection"
        );
        assert_eq!(
            broker
                .store()
                .meta_get(ARTIFACT_SWEEP_WORKER_STATUS_KEY)
                .unwrap()
                .as_deref(),
            Some(oversized_status.as_str()),
            "oversized legacy text remains unchanged"
        );
        broker
            .store()
            .meta_set(
                ARTIFACT_SWEEP_WORKER_STATUS_KEY,
                "complete passes=1 entries=0",
            )
            .unwrap();

        broker
            .store()
            .meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "7")
            .unwrap();
        assert_eq!(
            broker.legacy_artifact_sweep_state(),
            ArtifactSweepLegacyUnscopedState::UnknownPending,
            "an outstanding legacy request remains visible as unresolved"
        );
        assert_eq!(
            broker
                .store()
                .meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("7")
        );
        broker
            .store()
            .meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "5")
            .unwrap();
        assert_eq!(
            broker.legacy_artifact_sweep_state(),
            ArtifactSweepLegacyUnscopedState::Clear,
            "the BLOB control begins with otherwise clear legacy state"
        );

        let blob = vec![0xff; 16];
        broker
            .store()
            .connection_mut()
            .execute(
                "UPDATE meta SET value = ?2 WHERE key = ?1",
                rusqlite::params![
                    ARTIFACT_SWEEP_CURSOR_KEY,
                    rusqlite::types::Value::Blob(blob.clone()),
                ],
            )
            .unwrap();
        assert_eq!(
            broker.legacy_artifact_sweep_state(),
            ArtifactSweepLegacyUnscopedState::UnknownPending,
            "unreadable legacy data is retained and reported as unresolved"
        );
        let raw_value: rusqlite::types::Value = broker
            .store()
            .connection()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [ARTIFACT_SWEEP_CURSOR_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw_value, rusqlite::types::Value::Blob(blob));
    }

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
        let preparation_cache =
            std::fs::read_to_string(detached_fixture_marker(root, "preparation-cache")).unwrap();
        let root = root
            .canonicalize()
            .expect("canonicalize detached fixture root");
        command
            .args([
                "--exact",
                "gc::tests::detached_artifact_worker_child_fixture",
                "--nocapture",
            ])
            .env(DETACHED_FIXTURE_MODE_ENV, mode)
            .env(DETACHED_FIXTURE_ROOT_ENV, root)
            .env(ARTIFACT_SWEEP_WORKER_ENV, "1")
            .env_remove(crate::BROKER_DB_ENV)
            .env_remove("AETHYME_TEST_DB_SUFFIX")
            .env_remove("AETHYME_GATE_BROKER_DATABASES")
            .env_remove("AETHYME_PREPARATION_CACHE_ROOT")
            .env_remove("AETHYME_WORKTREE_ROOT")
            .env_remove("AETHYME_BROKER_STORAGE_CONTEXT_V1")
            .env_remove("AETHYME_BROKER_OPEN_HOST_OPERATION_DB")
            .env_remove("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT")
            .env_remove("AETHYME_BROKER_OPEN_WORKTREE_ROOT")
            .env_remove("AETHYME_BROKER_OPEN_COLLABORATION_ROOT")
            .env_remove(crate::disk_headroom::TEST_AVAILABLE_BYTES_ENV)
            .env_remove(crate::disk_headroom::TEST_AVAILABLE_INODES_ENV)
            .env("AETHYME_HOST_STATE_DIR", host_state.trim())
            .env("AETHYME_HOST_CACHE_DIR", host_cache.trim())
            .env("AETHYME_PREPARATION_CACHE_ROOT", preparation_cache.trim())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn open_detached_fixture_broker(root: &Path) -> Broker {
        let marker_path = |name| {
            PathBuf::from(
                std::fs::read_to_string(detached_fixture_marker(root, name))
                    .unwrap()
                    .trim(),
            )
        };
        let state = marker_path("host-state");
        let options = crate::BrokerOpenOptions::default()
            .with_host_state_dir(&state)
            .with_host_cache_dir(marker_path("host-cache"))
            .with_preparation_cache_root(marker_path("preparation-cache"))
            .with_host_operation_database(state.join("host-operations.db"))
            .with_worktree_root(marker_path("worktree-root"))
            .with_collaboration_state(state.join("collaboration"));
        Broker::open_with_options(root, options).unwrap()
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
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "7")
            .unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "3")
            .unwrap();
        let policy = load_retention_policy(root).unwrap();

        broker
            .run_artifact_sweep_recovery_worker_loop(
                &policy,
                7,
                None,
                None,
                |_broker, _policy, _remaining| {
                    Ok((
                        ArtifactSweepOutcome {
                            directories_reclaimed: 0,
                            complete: false,
                            auto_cleanup: None,
                        },
                        0,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("7"),
            "a stalled cache removal leaves its durable request pending"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("3")
        );
        assert!(test_worker_retry_pending(&mut broker, now_ms()).unwrap());

        broker.persist_artifact_sweep_recovery_request().unwrap();
        let retry_generation = broker
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
            .unwrap()
            .unwrap();
        assert!(retry_generation.parse::<i64>().unwrap() > 7);
        assert!(!test_worker_retry_pending(&mut broker, now_ms()).unwrap());
        broker
            .run_artifact_sweep_recovery_worker_loop(
                &policy,
                retry_generation.parse().unwrap(),
                None,
                None,
                |_broker, _policy, _remaining| {
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
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some(retry_generation.as_str()),
            "a subsequent eligible pass records the requested generation"
        );
        assert!(!test_worker_retry_pending(&mut broker, now_ms()).unwrap());
    }

    #[test]
    fn whole_checkout_requests_require_an_inline_or_drain_deferral() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nartifact_sweep_budget_ms = 0\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();

        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "9")
            .unwrap();
        assert!(
            !broker.auto_checkout_cleanup_recovery_requested().unwrap(),
            "a cache-only request does not create checkout-cleanup authority"
        );

        let deferred = crate::auto_cleanup::AutoCleanupReport {
            enabled: true,
            deferred: 1,
            ..crate::auto_cleanup::AutoCleanupReport::default()
        };
        for scope in [
            ArtifactSweepScope::FinishDrain,
            ArtifactSweepScope::AutomaticContinuation,
            ArtifactSweepScope::ClosedSession(7),
        ] {
            broker
                .persist_auto_checkout_cleanup_request(scope, &deferred)
                .unwrap();
            assert!(
                !broker.auto_checkout_cleanup_recovery_requested().unwrap(),
                "{scope:?} must not queue whole-checkout cleanup"
            );
        }
        broker
            .persist_auto_checkout_cleanup_request(
                ArtifactSweepScope::Inline,
                &crate::auto_cleanup::AutoCleanupReport {
                    enabled: true,
                    deferred: 0,
                    ..crate::auto_cleanup::AutoCleanupReport::default()
                },
            )
            .unwrap();
        assert!(
            !broker.auto_checkout_cleanup_recovery_requested().unwrap(),
            "a pass with no deferred candidates must not queue cleanup"
        );
        broker
            .persist_auto_checkout_cleanup_request(
                ArtifactSweepScope::Inline,
                &crate::auto_cleanup::AutoCleanupReport {
                    enabled: false,
                    deferred: 1,
                    ..crate::auto_cleanup::AutoCleanupReport::default()
                },
            )
            .unwrap();
        assert!(
            !broker.auto_checkout_cleanup_recovery_requested().unwrap(),
            "a disabled cleanup policy cannot create a worker request"
        );

        let deferred_retirement = crate::auto_cleanup::AutoCleanupReport {
            enabled: false,
            deferred: 1,
            retirement_deferred_reasons: vec![
                crate::auto_cleanup::SessionRetirementDeferredReason::MoreSessionsRemain {
                    after_session_id: 3,
                    high_water_session_id: 4,
                },
            ],
            ..crate::auto_cleanup::AutoCleanupReport::default()
        };
        for scope in [
            ArtifactSweepScope::FinishDrain,
            ArtifactSweepScope::AutomaticContinuation,
            ArtifactSweepScope::ClosedSession(7),
        ] {
            broker
                .persist_auto_checkout_cleanup_request(scope, &deferred_retirement)
                .unwrap();
            assert!(
                !broker.session_retirement_recovery_requested().unwrap(),
                "{scope:?} must not queue session retirement"
            );
        }
        broker
            .persist_auto_checkout_cleanup_request(ArtifactSweepScope::Inline, &deferred_retirement)
            .unwrap();
        let (retirement_generation, _) = broker.session_retirement_request_state().unwrap();
        assert!(retirement_generation > 0);
        assert!(broker.session_retirement_recovery_requested().unwrap());
        assert!(!broker.auto_checkout_cleanup_recovery_requested().unwrap());
        let (raw, mut retirement_state) = broker.load_session_retirement_state().unwrap();
        retirement_state.completed_generation = retirement_generation;
        assert!(
            broker
                .store_session_retirement_state(raw.as_deref(), &retirement_state)
                .unwrap()
        );

        broker
            .persist_auto_checkout_cleanup_request(ArtifactSweepScope::Drain, &deferred)
            .unwrap();
        let first_generation = broker
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)
            .unwrap()
            .expect("ordinary deferrals persist a separate generation");
        assert!(broker.auto_checkout_cleanup_recovery_requested().unwrap());

        let retry_after = now_ms().saturating_add(60_000).to_string();
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY, &retry_after)
            .unwrap();
        broker
            .persist_auto_checkout_cleanup_request(ArtifactSweepScope::Inline, &deferred)
            .unwrap();
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some(first_generation.as_str()),
            "an incomplete request remains idempotent"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY)
                .unwrap()
                .as_deref(),
            Some(retry_after.as_str()),
            "repeated deferral cannot bypass the lane backoff"
        );

        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY, &first_generation)
            .unwrap();
        broker
            .persist_auto_checkout_cleanup_request(ArtifactSweepScope::Drain, &deferred)
            .unwrap();
        let next_generation = broker
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)
            .unwrap()
            .unwrap();
        assert!(next_generation.parse::<i64>().unwrap() > first_generation.parse::<i64>().unwrap());
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY)
                .unwrap()
                .as_deref(),
            Some(""),
            "a new request after completion clears only its own lane backoff"
        );
    }

    #[test]
    fn checkout_worker_discovery_progress_survives_zero_removal_passes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        let connection = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        let closed_at = now_ms();
        for index in 0..289 {
            let worktree = root.join(format!("retained-checkout-{index}"));
            std::fs::create_dir_all(&worktree).unwrap();
            connection
                .execute(
                    "INSERT INTO sessions (worktree_path, branch, origin, status,
                     cleanup_state, closed_at, created_at, updated_at, last_activity_at)
                 VALUES (?1, ?2, 'adopted', 'cleaned', 'closed', ?3, ?3, ?3, ?3)",
                    rusqlite::params![
                        worktree.to_string_lossy(),
                        format!("agent/history-{index}"),
                        closed_at,
                    ],
                )
                .unwrap();
        }
        drop(connection);
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY, "1")
            .unwrap();
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY, "0")
            .unwrap();
        let mut passes = 0;
        let mut removed = 0usize;
        broker
            .run_auto_checkout_cleanup_recovery_worker_loop(
                1,
                Some(AUTO_CHECKOUT_CLEANUP_WORKER_MAX_PASSES),
                Some(Duration::from_secs(60)),
                |broker, generation, remaining| {
                    passes += 1;
                    let Some(remaining) = remaining else {
                        return Ok(None);
                    };
                    let pass = broker.auto_checkout_cleanup_recovery_pass(
                        generation,
                        Instant::now() + remaining,
                    )?;
                    if let Some((report, _)) = &pass {
                        removed = removed.saturating_add(report.removed.len());
                    }
                    Ok(pass)
                },
            )
            .unwrap();
        assert!(
            passes > ARTIFACT_SWEEP_WORKER_NO_PROGRESS_PASSES,
            "discovery must continue beyond the old zero-removal backoff threshold"
        );
        assert!(
            passes > 8,
            "worker must continue past the former eight-pass worker limit"
        );
        assert_eq!(removed, 0, "the fixture must exercise discovery only");
        let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
        let group_count = broker
            .store()
            .cleanup_cohort_group_count(namespace, 0, i64::MAX)
            .unwrap();
        assert_eq!(group_count, 289, "the fixture must produce 289 groups");
        let group_cursor = broker
            .store()
            .meta_get("auto_cleanup.cursor")
            .unwrap()
            .expect("the production planner must persist its group cursor");
        let group_cursor: serde_json::Value = serde_json::from_str(&group_cursor).unwrap();
        let group_cursor_after = group_cursor["after_session_id"].as_i64().unwrap();
        let group_cursor_high_water = group_cursor["high_water_session_id"].as_i64().unwrap();
        assert!(
            group_cursor_high_water > 256,
            "the group page limit is 32, so this must cover more than eight pages"
        );
        assert_eq!(
            group_cursor_after, group_cursor_high_water,
            "the group cursor must reach the last of more than eight planner pages"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("1"),
            "the production cleanup planner must reach the end of all group pages"
        );
        let state = broker
            .store()
            .cleanup_cohort_scan_state(namespace)
            .unwrap()
            .unwrap();
        assert!(state.complete, "all history rows must be discovered");
        assert!(
            !broker
                .auto_checkout_cleanup_retry_pending(now_ms())
                .unwrap(),
            "actual discovery progress must not be treated as a stalled worker"
        );
    }

    #[cfg(unix)]
    #[test]
    fn retained_checkout_outbox_reconciliation_preserves_cooldown() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        let stale = broker
            .start_worktree("stale open retirement candidate", None)
            .unwrap();
        age_open_session_for_retirement(&mut broker, stale.id, Path::new(&stale.worktree_path));
        let connection = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        let future_checkout_cleanup = now_ms().saturating_add(24 * 60 * 60 * 1_000);
        connection
            .execute(
                "INSERT INTO sessions (worktree_path, branch, origin, status, cleanup_state,
                 created_at, updated_at, last_activity_at, closed_at)
             VALUES (?1, 'agent/retained', 'spawned', 'cleaned', 'closed', 1, 1, 1, 1)",
                [root.join("retained-checkout").to_string_lossy().as_ref()],
            )
            .unwrap();
        let session_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO closed_checkout_cleanup_requests
                 (session_id, requested_at_ms, not_before_ms, last_progress_at_ms)
             VALUES (?1, 1, 1, 1)",
                [session_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE closed_checkout_cleanup_requests SET not_before_ms = ?1
                 WHERE session_id = ?2",
                rusqlite::params![future_checkout_cleanup, session_id],
            )
            .unwrap();
        drop(connection);
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY, "7")
            .unwrap();
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY, "7")
            .unwrap();
        let retry_after = future_checkout_cleanup.to_string();
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY, &retry_after)
            .unwrap();
        broker.reconcile_checkout_cleanup_outbox().unwrap();
        broker.reconcile_checkout_cleanup_outbox().unwrap();
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_RETRY_AFTER_KEY)
                .unwrap()
                .as_deref(),
            Some(retry_after.as_str())
        );
        assert!(
            broker
                .auto_checkout_cleanup_retry_pending(now_ms())
                .unwrap()
        );
        assert!(
            broker
                .store()
                .pending_checkout_cleanup_request(session_id)
                .unwrap()
                .unwrap()
                .completed_at_ms
                .is_none(),
            "retained checkout must remain an outstanding obligation"
        );

        let policy = load_retention_policy(root).unwrap();
        let retirement_generation = broker
            .request_session_retirement_if_due(&policy, ArtifactSweepScope::Inline)
            .unwrap()
            .expect("session retirement has an independent request");
        assert!(
            broker
                .auto_checkout_cleanup_retry_pending(now_ms())
                .unwrap(),
            "the physical checkout lane remains in cooldown"
        );
        assert!(
            broker
                .session_retirement_due_generation(now_ms())
                .unwrap()
                .is_some(),
            "retirement admission does not inherit checkout cooldown"
        );
        broker
            .run_session_retirement_recovery_worker_loop(
                retirement_generation,
                Some(SESSION_RETIREMENT_WORKER_MAX_PASSES),
                Some(Duration::from_secs(5)),
            )
            .unwrap();
        assert_eq!(
            broker.store().session(stale.id).unwrap().cleanup_state,
            crate::SessionCleanupState::Closed,
            "the stale open session retires despite a future closed-checkout cleanup"
        );
        assert!(
            broker
                .store()
                .pending_checkout_cleanup_request(session_id)
                .unwrap()
                .unwrap()
                .completed_at_ms
                .is_none(),
            "retirement does not consume the closed checkout's physical-removal request"
        );
        assert!(
            broker
                .auto_checkout_cleanup_retry_pending(now_ms())
                .unwrap(),
            "retirement leaves the physical-removal cooldown intact"
        );
    }

    #[cfg(unix)]
    #[test]
    fn zero_artifact_budget_queues_retirement_even_when_gc_lock_is_busy() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 72\nartifact_sweep_budget_ms = 0\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        let stale = broker
            .start_worktree("zero-budget retirement candidate", None)
            .unwrap();
        age_open_session_for_retirement(&mut broker, stale.id, Path::new(&stale.worktree_path));
        broker
            .store()
            .meta_set(
                SESSION_RETIREMENT_STATE_KEY,
                &serde_json::to_string(&SessionRetirementRequestState::default()).unwrap(),
            )
            .unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_STAMP_KEY, &now_ms().to_string())
            .unwrap();
        let policy = load_retention_policy(root).unwrap();
        let lock = GcLock::acquire(root).unwrap();

        let (outcome, entries) = broker
            .sweep_artifacts_autonomously_with_progress(&policy, ArtifactSweepScope::Inline)
            .unwrap();
        assert!(outcome.complete);
        assert!(outcome.auto_cleanup.is_none());
        assert_eq!(entries, 0);
        assert!(
            broker.session_retirement_recovery_requested().unwrap(),
            "the no-cache pass leaves an independent retirement request while another GC holds the lock"
        );
        let request_generation = broker.session_retirement_request_state().unwrap().0;
        assert!(request_generation > 0);
        drop(lock);

        broker
            .run_session_retirement_recovery_worker_loop(
                request_generation,
                Some(SESSION_RETIREMENT_WORKER_MAX_PASSES),
                Some(Duration::from_secs(5)),
            )
            .unwrap();
        assert_eq!(
            broker.store().session(stale.id).unwrap().cleanup_state,
            crate::SessionCleanupState::Closed
        );
        assert_eq!(
            broker.session_retirement_request_state().unwrap(),
            (request_generation, request_generation),
            "the worker completes only the queued generation"
        );
    }

    #[test]
    fn stale_session_retirement_completion_cannot_ack_a_newer_cas_generation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        let policy = load_retention_policy(root).unwrap();
        let lock = GcLock::acquire(root).unwrap();

        let generation = broker
            .request_session_retirement_if_due(&policy, ArtifactSweepScope::Inline)
            .unwrap()
            .expect("request CAS must not wait for the lifecycle lock");
        let (raw, mut state) = broker.load_session_retirement_state().unwrap();
        state.requested_generation = generation.checked_add(1).unwrap();
        state.completed_generation = generation.saturating_sub(1);
        state.retry_after_ms = 123;
        assert!(
            broker
                .store_session_retirement_state(raw.as_deref(), &state)
                .unwrap()
        );

        broker
            .complete_session_retirement_request(generation)
            .unwrap();
        let (_, after) = broker.load_session_retirement_state().unwrap();
        assert_eq!(after.requested_generation, generation + 1);
        assert_eq!(after.completed_generation, generation - 1);
        assert_eq!(after.retry_after_ms, 123);

        let invalid_state = SessionRetirementRequestState {
            version: SESSION_RETIREMENT_STATE_VERSION.saturating_add(1),
            requested_generation: generation + 2,
            completed_generation: generation + 1,
            retry_after_ms: 0,
        };
        let invalid_raw = serde_json::to_string(&invalid_state).unwrap();
        broker
            .store()
            .meta_set(SESSION_RETIREMENT_STATE_KEY, &invalid_raw)
            .unwrap();
        assert_eq!(
            broker
                .request_session_retirement_if_due(&policy, ArtifactSweepScope::Inline)
                .unwrap(),
            None,
            "an unknown state version is not reset into retirement authority"
        );
        assert_eq!(
            broker.session_retirement_recovery_state().unwrap(),
            None,
            "an invalid private lane does not claim a pending or completed request"
        );
        assert_eq!(
            broker
                .store()
                .meta_get(SESSION_RETIREMENT_STATE_KEY)
                .unwrap()
                .as_deref(),
            Some(invalid_raw.as_str()),
            "unknown state remains fail-closed and inspectable"
        );
        drop(lock);
    }

    #[test]
    fn stale_checkout_generation_cannot_run_or_complete_a_newer_request() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY, "8")
            .unwrap();
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY, "3")
            .unwrap();

        broker
            .run_auto_checkout_cleanup_recovery_worker_loop(
                7,
                Some(1),
                Some(Duration::from_secs(1)),
                |broker, generation, pass_budget| {
                    let deadline = Instant::now()
                        + pass_budget.unwrap_or(AUTO_CHECKOUT_CLEANUP_WORKER_PASS_BUDGET);
                    broker.auto_checkout_cleanup_recovery_pass(generation, deadline)
                },
            )
            .unwrap();

        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("8"),
            "a stale pass cannot replace the newer request generation"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("3"),
            "a stale pass cannot complete the newer request"
        );
        assert!(
            broker
                .auto_checkout_cleanup_retry_pending(now_ms())
                .unwrap()
        );
    }

    #[test]
    fn cache_lane_error_still_dispatches_due_checkout_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY, "7")
            .unwrap();
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY, "3")
            .unwrap();
        let mut cleanup_dispatched = false;

        broker
            .run_cleanup_after_cache_lane(
                Err(BrokerOpError::RetentionConfig(
                    crate::RetentionConfigError::Parse("invalid fixture retention policy".into()),
                )),
                |broker| {
                    cleanup_dispatched = true;
                    assert!(broker.auto_checkout_cleanup_recovery_requested()?);
                    Ok(())
                },
            )
            .unwrap();

        assert!(cleanup_dispatched);
        assert!(test_worker_retry_pending(&mut broker, now_ms()).unwrap());
        assert!(broker.auto_checkout_cleanup_recovery_requested().unwrap());
        assert!(
            !broker
                .auto_checkout_cleanup_retry_pending(now_ms())
                .unwrap()
        );

        let mut cleanup_dispatched_after_db_error = false;
        let result = broker.run_cleanup_after_cache_lane(
            Err(BrokerOpError::Store(crate::BrokerError::Sqlite(
                rusqlite::Error::InvalidQuery,
            ))),
            |_broker| {
                cleanup_dispatched_after_db_error = true;
                Ok(())
            },
        );
        assert!(result.is_err(), "database errors remain fatal");
        assert!(!cleanup_dispatched_after_db_error);
    }

    #[test]
    fn checkout_cleanup_timeout_keeps_request_and_cursor_durable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY, "7")
            .unwrap();
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY, "3")
            .unwrap();
        let cursor = r#"{"version":1,"after_session_id":4,"high_water_session_id":9}"#;
        broker
            .store()
            .meta_set("auto_cleanup.cursor", cursor)
            .unwrap();

        broker
            .run_auto_checkout_cleanup_recovery_worker_loop(
                7,
                Some(8),
                Some(Duration::from_millis(10)),
                |_broker, _generation, pass_budget| {
                    assert!(pass_budget.is_some());
                    std::thread::sleep(Duration::from_millis(20));
                    Ok(Some((
                        crate::auto_cleanup::AutoCleanupReport {
                            enabled: true,
                            deferred: 1,
                            ..crate::auto_cleanup::AutoCleanupReport::default()
                        },
                        false,
                    )))
                },
            )
            .unwrap();

        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("7")
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("3"),
            "a timed-out cleanup pass cannot complete its request"
        );
        assert_eq!(
            broker
                .store()
                .meta_get("auto_cleanup.cursor")
                .unwrap()
                .as_deref(),
            Some(cursor),
            "the worker preserves the durable fair cursor for its next attempt"
        );
        assert!(
            broker
                .auto_checkout_cleanup_retry_pending(now_ms())
                .unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn whole_checkout_worker_uses_its_request_and_leaves_gc_journal_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let origin_text = origin.to_string_lossy().into_owned();
        let publisher = tmp.path().join("publisher");
        let repo = tmp.path().join("repo");
        git(
            tmp.path(),
            &["init", "-q", "--bare", "-b", "main", "origin.git"],
        );
        git(
            tmp.path(),
            &["clone", "-q", origin_text.as_str(), "publisher"],
        );
        std::fs::write(publisher.join(".gitignore"), "/.aethyme/\ntarget/\n.env\n").unwrap();
        std::fs::write(publisher.join("README.md"), "fixture\n").unwrap();
        git(&publisher, &["add", "-A"]);
        git(&publisher, &["commit", "-qm", "fixture"]);
        git(&publisher, &["push", "-q", "origin", "main"]);
        git(tmp.path(), &["clone", "-q", origin_text.as_str(), "repo"]);
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(
            repo.join(".aethyme/broker.toml"),
            "[retention]\nartifact_sweep_budget_ms = 0\nclosed_worktree_grace_hours = 0\n",
        )
        .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repo).unwrap();
        let session = broker
            .start_worktree("merged worker candidate", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("feature.txt"), "landed\n").unwrap();
        git(&worktree, &["add", "feature.txt"]);
        git(&worktree, &["commit", "-qm", "feature"]);
        let remote_branch = format!("HEAD:refs/heads/{}", session.branch);
        git(&worktree, &["push", "-q", "origin", remote_branch.as_str()]);
        git(&publisher, &["fetch", "-q", "origin"]);
        let remote_branch = format!("origin/{}", session.branch);
        git(
            &publisher,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "merge",
                remote_branch.as_str(),
            ],
        );
        git(&publisher, &["push", "-q", "origin", "main"]);
        git(&repo, &["fetch", "-q", "origin"]);
        broker.close(session.id).unwrap();

        let journal_path = repo.join(".aethyme/gc-journal.json");
        let journal = GcJournal::from(broker.gc_plan().unwrap());
        write_journal(&journal_path, &journal).unwrap();
        let journal_before = std::fs::read(&journal_path).unwrap();
        let deferred = crate::auto_cleanup::AutoCleanupReport {
            enabled: true,
            deferred: 1,
            ..crate::auto_cleanup::AutoCleanupReport::default()
        };
        broker
            .artifact_sweep_meta_set(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY, "0")
            .unwrap();
        broker
            .persist_auto_checkout_cleanup_request(ArtifactSweepScope::Drain, &deferred)
            .unwrap();
        let first_generation = broker
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)
            .unwrap()
            .unwrap()
            .parse::<i64>()
            .unwrap();

        let cleanup_config = repo.join(".aethyme/config.toml");
        std::fs::write(&cleanup_config, "[cleanup]\nauto_remove = \"invalid\"\n").unwrap();
        let (invalid_report, invalid_completed) = broker
            .auto_checkout_cleanup_recovery_pass(first_generation, Instant::now())
            .unwrap()
            .expect("the invalid-policy pass acquires the cleanup lock");
        assert!(invalid_report.config_error.is_some());
        assert!(!invalid_completed);
        assert!(worktree.is_dir());
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("0"),
            "an invalid cleanup policy leaves its request pending"
        );

        std::fs::write(&cleanup_config, "[cleanup]\nauto_remove = false\n").unwrap();
        let (disabled_report, disabled_completed) = broker
            .auto_checkout_cleanup_recovery_pass(
                first_generation,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap()
            .expect("the disabled-policy pass acquires the cleanup lock");
        assert!(!disabled_report.enabled);
        assert_eq!(disabled_report.deferred, 0);
        assert!(disabled_completed);
        assert!(worktree.is_dir(), "disabled policy keeps the checkout");
        broker
            .persist_auto_checkout_cleanup_request(
                ArtifactSweepScope::Drain,
                &crate::auto_cleanup::AutoCleanupReport {
                    enabled: false,
                    deferred: 1,
                    ..crate::auto_cleanup::AutoCleanupReport::default()
                },
            )
            .unwrap();
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some(first_generation.to_string().as_str()),
            "disabled cleanup cannot enqueue another generation"
        );

        std::fs::write(&cleanup_config, "[cleanup]\nauto_remove = true\n").unwrap();
        broker
            .persist_auto_checkout_cleanup_request(ArtifactSweepScope::Inline, &deferred)
            .unwrap();
        let generation = broker
            .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_REQUESTED_KEY)
            .unwrap()
            .unwrap()
            .parse::<i64>()
            .unwrap();
        assert!(generation > first_generation);
        broker
            .run_auto_checkout_cleanup_recovery_worker_loop(
                generation,
                Some(ARTIFACT_SWEEP_WORKER_MAX_PASSES),
                Some(Duration::from_secs(60)),
                |broker, generation, pass_budget| {
                    let deadline = Instant::now()
                        + pass_budget.unwrap_or(AUTO_CHECKOUT_CLEANUP_WORKER_PASS_BUDGET);
                    broker.auto_checkout_cleanup_recovery_pass(generation, deadline)
                },
            )
            .unwrap();

        assert!(
            !worktree.exists(),
            "the worker removes the eligible checkout through the existing proof path"
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            crate::SessionCleanupState::Cleaned
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(AUTO_CHECKOUT_CLEANUP_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some(generation.to_string().as_str()),
            "the exact cleanup request is completed after removal"
        );
        assert_eq!(
            std::fs::read(&journal_path).unwrap(),
            journal_before,
            "whole-checkout cleanup does not resume or rewrite an operator GC journal"
        );
    }

    #[test]
    fn positive_partial_worker_progress_is_capped_and_later_retry_completes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "7")
            .unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "3")
            .unwrap();
        let policy = load_retention_policy(root).unwrap();
        let mut passes = 0;

        broker
            .run_artifact_sweep_recovery_worker_loop(
                &policy,
                7,
                Some(2),
                None,
                |_broker, _policy, _remaining| {
                    passes += 1;
                    Ok((
                        ArtifactSweepOutcome {
                            directories_reclaimed: 1,
                            complete: false,
                            auto_cleanup: None,
                        },
                        1,
                    ))
                },
            )
            .unwrap();
        assert_eq!(passes, 2, "positive progress cannot bypass the pass cap");
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("3"),
            "partial work must not advance the completed request generation"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("7"),
            "the request remains durable across the bounded worker exit"
        );
        assert!(test_worker_retry_pending(&mut broker, now_ms()).unwrap());
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_WORKER_STATUS_KEY)
                .unwrap()
                .as_deref(),
            Some("pass_limit passes=2 entries=2")
        );

        broker.persist_artifact_sweep_recovery_request().unwrap();
        let retry_generation = broker
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
            .unwrap()
            .unwrap();
        broker
            .run_artifact_sweep_recovery_worker_loop(
                &policy,
                retry_generation.parse().unwrap(),
                Some(1),
                None,
                |_broker, _policy, _remaining| {
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
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some(retry_generation.as_str()),
            "a later eligible pass completes the retained request"
        );
    }

    #[test]
    fn worker_wall_clock_limit_keeps_request_pending_after_positive_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "7")
            .unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "3")
            .unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_CURSOR_KEY, "4")
            .unwrap();
        let policy = load_retention_policy(root).unwrap();
        let mut passes = 0;

        broker
            .run_artifact_sweep_recovery_worker_loop(
                &policy,
                7,
                None,
                Some(Duration::from_millis(1)),
                |_broker, _policy, _remaining| {
                    passes += 1;
                    std::thread::sleep(Duration::from_millis(2));
                    Ok((
                        ArtifactSweepOutcome {
                            directories_reclaimed: 0,
                            complete: false,
                            auto_cleanup: None,
                        },
                        1,
                    ))
                },
            )
            .unwrap();

        assert_eq!(
            passes, 1,
            "positive progress cannot exceed the time ceiling"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                .unwrap()
                .as_deref(),
            Some("7")
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("3"),
            "partial work never advances the completed request generation"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some("4"),
            "the timeout preserves the cursor for a later worker"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_WORKER_STATUS_KEY)
                .unwrap()
                .as_deref(),
            Some("time_limit passes=1 entries=1")
        );
        assert!(test_worker_retry_pending(&mut broker, now_ms()).unwrap());
    }

    fn detached_fixture_store(root: &Path) -> crate::BrokerStore {
        crate::BrokerStore::open(&root.join(".aethyme/broker.db")).unwrap()
    }

    fn detached_fixture_namespace(root: &Path) -> String {
        let store = detached_fixture_store(root);
        let registry = store
            .meta_get("gc.artifact_sweep.v2.context_registry")
            .unwrap()
            .expect("the setup child registers its captured context");
        let namespaces: Vec<String> = serde_json::from_str(&registry).unwrap();
        assert_eq!(namespaces.len(), 1);
        namespaces[0].clone()
    }

    #[test]
    fn test_harness_executable_is_never_selected_as_the_detached_runtime() {
        assert!(
            artifact_worker_binaries().is_none(),
            "Cargo test executables must never be launched as installed workers"
        );
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

    #[cfg(unix)]
    fn age_open_session_for_retirement(
        broker: &mut crate::Broker,
        session_id: i64,
        worktree_path: &Path,
    ) {
        let missing_worktree =
            worktree_path.with_file_name(format!("missing-retirement-worktree-{session_id}"));
        let old = now_ms().saturating_sub(80 * 60 * 60 * 1_000);
        let connection = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        connection
            .execute(
                "UPDATE sessions SET created_at = ?1, updated_at = ?1,
                 last_activity_at = ?1, worktree_path = ?2 WHERE id = ?3",
                rusqlite::params![old, missing_worktree.to_string_lossy().as_ref(), session_id],
            )
            .unwrap();
    }

    #[test]
    fn bounded_file_hash_stops_after_a_partial_gate_log_read() {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(1);
        let content = vec![b'x'; GC_FILE_READ_CHUNK_BYTES * 3];
        let mut reader = std::io::Cursor::new(content.clone());
        let mut clock_calls = 0;
        let result = sha256_reader_until_with_clock(&mut reader, Some(deadline), || {
            clock_calls += 1;
            if clock_calls == 1 { started } else { deadline }
        })
        .unwrap();

        assert!(
            result.is_none(),
            "a partial digest is not a file fingerprint"
        );
        assert_eq!(
            reader.position(),
            GC_FILE_READ_CHUNK_BYTES as u64,
            "the reader stops before consuming a second chunk"
        );

        let mut retry = std::io::Cursor::new(content.clone());
        let result = sha256_reader_until_with_clock(
            &mut retry,
            Some(Instant::now() + Duration::from_secs(5)),
            Instant::now,
        )
        .unwrap()
        .expect("a fresh positive pass completes after the partial pass");
        assert_eq!(result.0, sha256(&content));
        assert_eq!(result.1, content.len() as u64);
    }

    #[test]
    fn bounded_metrics_read_stops_before_returning_partial_contents() {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(1);
        let content = vec![b'x'; GC_FILE_READ_CHUNK_BYTES * 3];
        let mut reader = std::io::Cursor::new(content.clone());
        let mut clock_calls = 0;
        let result = read_all_until_with_clock(&mut reader, Some(deadline), || {
            clock_calls += 1;
            if clock_calls == 1 { started } else { deadline }
        })
        .unwrap();

        assert!(
            result.is_none(),
            "partial metrics cannot produce a rewrite plan"
        );
        assert_eq!(reader.position(), GC_FILE_READ_CHUNK_BYTES as u64);

        let mut retry = std::io::Cursor::new(content.clone());
        let complete = read_all_until_with_clock(
            &mut retry,
            Some(Instant::now() + Duration::from_secs(5)),
            Instant::now,
        )
        .unwrap()
        .expect("a fresh positive pass completes after the partial pass");
        assert_eq!(complete, content);
    }

    #[test]
    fn full_metrics_rewrite_preserves_per_malformed_line_blockers_and_output() {
        let tmp = tempfile::tempdir().unwrap();
        let directory = tmp.path().join(".aethyme/logs");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("command-metrics.jsonl");
        let before = b"{\"ts\":1}\nnot-json\n{\"ts\":3}\n{}\n";
        let after = b"not-json\n{\"ts\":3}\n{}\n";
        std::fs::write(&path, before).unwrap();
        let mut blockers = Vec::new();

        let MetricsRewrite::Complete(Some(candidate)) =
            metrics_rewrite(tmp.path(), 2, &mut blockers, None).unwrap()
        else {
            panic!("old metrics should yield a complete rewrite candidate");
        };

        let expected_after_sha256 = sha256(after);
        assert_eq!(candidate.before_sha256, sha256(before));
        assert_eq!(
            candidate.after_sha256.as_deref(),
            Some(expected_after_sha256.as_str())
        );
        assert_eq!(retained_metric_bytes(before, 2), after);
        assert_eq!(
            blockers
                .iter()
                .filter(|blocker| blocker.kind == "command_metric_line")
                .count(),
            2,
            "full GC retains its historical per-line blocker count"
        );
    }

    #[test]
    fn gc_age_anchor_stabilizes_near_cutoff_metrics_and_is_in_the_plan_digest() {
        let days = 30;
        let day_start = 40 * MILLIS_PER_DAY;
        let evaluated_first = day_start + MILLIS_PER_DAY / 4;
        let evaluated_later = day_start + 3 * MILLIS_PER_DAY / 4;
        let age_anchor_ms = utc_day_floor_ms(evaluated_first);
        assert_eq!(utc_day_floor_ms(evaluated_later), age_anchor_ms);
        assert_ne!(cutoff(evaluated_first, days), cutoff(evaluated_later, days));

        let stable_cutoff = cutoff(age_anchor_ms, days);
        let within_day = stable_cutoff + MILLIS_PER_DAY / 2;
        let boundary_line = format!("{{\"ts\":{stable_cutoff},\"command\":\"boundary\"}}\n");
        let within_day_line = format!("{{\"ts\":{within_day},\"command\":\"within-day\"}}\n");
        let old_line = format!("{{\"ts\":{},\"command\":\"old\"}}\n", stable_cutoff - 1);
        let before = format!("{old_line}{within_day_line}{boundary_line}").into_bytes();

        let legacy_first = retained_metric_bytes(&before, cutoff(evaluated_first, days));
        let legacy_later = retained_metric_bytes(&before, cutoff(evaluated_later, days));
        assert_ne!(
            legacy_first, legacy_later,
            "the exact-time cutoff moves within the day"
        );

        let retained_first = retained_metric_bytes(&before, stable_cutoff);
        let retained_later =
            retained_metric_bytes(&before, cutoff(utc_day_floor_ms(evaluated_later), days));
        assert_eq!(retained_first, retained_later);
        assert_eq!(
            retained_first,
            format!("{within_day_line}{boundary_line}").as_bytes()
        );

        let next_day_anchor = age_anchor_ms + MILLIS_PER_DAY;
        let retained_next_day = retained_metric_bytes(&before, cutoff(next_day_anchor, days));
        assert!(
            retained_next_day.is_empty(),
            "the boundary row ages out only after the UTC-day cutoff advances"
        );

        let metric_candidate = |after: &[u8]| GcFileCandidate {
            path: ".aethyme/logs/command-metrics.jsonl".into(),
            action: GcFileAction::Rewrite,
            before_sha256: sha256(&before),
            after_sha256: Some(sha256(after)),
            bytes_before: before.len() as u64,
            bytes_after: after.len() as u64,
            source_row_ids: Vec::new(),
        };
        let make_plan = |evaluated_at: i64, anchor: i64, after: &[u8]| -> GcPlan {
            serde_json::from_value(serde_json::json!({
                "schema_version": GC_PLAN_SCHEMA_VERSION,
                "digest": "",
                "evaluated_at": evaluated_at,
                "age_anchor_ms": anchor,
                "policy": RetentionPolicy::default(),
                "rows": [],
                "files": [metric_candidate(after)],
                "worktrees": [],
                "artifacts": [],
                "orphans": [],
                "blockers": [],
                "estimated_reclaimable_bytes": 0,
                "estimated_retained_bytes": 0,
                "estimated_blocked_bytes": 0
            }))
            .unwrap()
        };
        let mut first_plan = make_plan(evaluated_first, age_anchor_ms, &retained_first);
        first_plan.finish_digest().unwrap();
        let mut later_plan = make_plan(evaluated_later, age_anchor_ms, &retained_later);
        later_plan.finish_digest().unwrap();
        assert_eq!(first_plan.digest, later_plan.digest);

        let mut changed_anchor = first_plan.clone();
        changed_anchor.age_anchor_ms = Some(next_day_anchor);
        changed_anchor.finish_digest().unwrap();
        assert_ne!(first_plan.digest, changed_anchor.digest);

        let mut next_day_plan = make_plan(evaluated_later, next_day_anchor, &retained_next_day);
        next_day_plan.finish_digest().unwrap();
        assert_ne!(first_plan.digest, next_day_plan.digest);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_file_open_refuses_a_fifo_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("gate.log");
        let encoded = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `encoded` is a NUL-terminated path into this live TempDir.
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);

        let started = Instant::now();
        let result = open_regular_file_nonblocking(&path);

        assert!(result.is_err(), "a FIFO is not a regular gate log");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "nonblocking open must not wait for a FIFO writer"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn an_expired_retention_deadline_marks_unknown_without_partial_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let plan = broker
            .gc_plan_scanned_until(
                crate::SizeScan::Recorded,
                false,
                Some(Instant::now() - Duration::from_millis(1)),
            )
            .unwrap();

        for expected in ["closed_sessions", "gc_rows", "gate_cache_inventory"] {
            assert!(
                plan.deferred_checks.iter().any(|check| check == expected),
                "expired inventory must report {expected} as deferred: {:?}",
                plan.deferred_checks
            );
        }
        assert_eq!(plan.budget_verdict, crate::BudgetVerdict::Unknown);
        assert!(!plan.clears_retained_bytes_budget);
        assert!(plan.rows.is_empty());
        assert!(plan.files.is_empty());
        assert!(plan.gate_caches.is_empty());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn expired_measured_gc_plan_has_no_apply_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let plan = broker
            .gc_plan_scanned_until(
                crate::SizeScan::Measure,
                false,
                Some(Instant::now() - Duration::from_millis(1)),
            )
            .unwrap();

        assert!(plan.digest.is_empty());
        assert!(
            plan.deferred_checks
                .iter()
                .any(|check| check == "plan_deadline")
        );
        assert!(plan.rows.is_empty());
        assert!(plan.files.is_empty());
        assert!(plan.worktrees.is_empty());
        assert!(plan.artifacts.is_empty());
        assert!(plan.orphans.is_empty());
        assert!(plan.gate_caches.is_empty());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn bounded_artifact_discovery_stops_after_its_entry_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        for index in 0..64 {
            std::fs::write(root.join(format!("fixture-{index}.txt")), "fixture\n").unwrap();
        }
        let checkout = GitRepo::discover(&root).unwrap();

        let (scan, complete) = collect_artifact_scan_until(
            &root,
            &[],
            &checkout,
            Instant::now() + Duration::from_secs(2),
            8,
        );

        assert!(!complete, "exceeding the entry budget is incomplete");
        assert!(scan.classified.is_empty());
        assert!(scan.declined.is_empty());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn bounded_artifact_discovery_defers_when_git_ignore_check_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.join("unclassified-output")).unwrap();
        let checkout = GitRepo::discover(&root).unwrap();
        std::fs::remove_dir_all(root.join(".git")).unwrap();

        let (scan, complete) = collect_artifact_scan_until(
            &root,
            &[],
            &checkout,
            Instant::now() + Duration::from_secs(2),
            64,
        );

        assert!(
            !complete,
            "a failed check-ignore is an incomplete inventory"
        );
        assert!(scan.classified.is_empty());
        assert!(scan.declined.is_empty());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn oversized_size_cache_is_deferred_without_overwriting_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        let runtime = root.join(".aethyme");
        std::fs::create_dir_all(&runtime).unwrap();
        let cache_path = runtime.join("worktree-sizes.json");
        // The bounded size-cache reader admits at most 8 MiB.
        let original = vec![b'x'; 8 * 1024 * 1024 + 1];
        std::fs::write(&cache_path, &original).unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let plan = broker
            .gc_plan_scanned_until(
                crate::SizeScan::Measure,
                false,
                Some(Instant::now() + Duration::from_secs(5)),
            )
            .unwrap();

        assert!(plan.digest.is_empty());
        assert!(
            plan.deferred_checks
                .iter()
                .any(|check| check == "cleanup_size_records")
        );
        assert_eq!(std::fs::read(cache_path).unwrap(), original);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn artifact_sweep_preserves_shared_consumers_with_or_without_a_bound_worktree_root() {
        for bound in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().canonicalize().unwrap();
            git(&root, &["init", "-q", "-b", "main"]);
            std::fs::write(root.join("README.md"), "fixture\n").unwrap();
            git(&root, &["add", "README.md"]);
            git(&root, &["commit", "-qm", "fixture"]);
            let storage = crate::test_storage::TestStorage::new();
            let mut broker = storage.open(&root).unwrap();
            if bound {
                broker
                    .start_worktree("bind only this repository", None)
                    .unwrap();
            }
            let cache_root = broker
                .storage_context()
                .preparation_cache_root()
                .unwrap()
                .unwrap();
            let other_repository = "b".repeat(64);
            let protected = cache_root.join(&other_repository).join("bbbbbbbbbbbb");
            std::fs::create_dir_all(&protected).unwrap();
            std::fs::write(protected.join("payload"), b"live in another repository").unwrap();
            let repository = "a".repeat(64);
            let store = cache_root.join(&repository);
            std::fs::create_dir_all(&store).unwrap();
            std::fs::write(store.join("old-cache"), b"witnessed rotation").unwrap();
            let resource_db_path = broker
                .storage_context()
                .host_resource_db_path
                .clone()
                .unwrap();
            let identity =
                crate::preparation::rotation_debt::DirectoryIdentity::capture(&cache_root).unwrap();
            let debt = crate::preparation::rotation_debt::RotationDebt::new(
                &repository,
                &crate::preparation::rotation_debt::new_transaction_id(),
                identity.clone(),
                crate::preparation::rotation_debt::DirectoryIdentity::capture(&store).unwrap(),
            )
            .unwrap();
            crate::preparation::rotation_journal::publish(
                &resource_db_path,
                &cache_root,
                &identity.encode().unwrap(),
                &repository,
                &crate::preparation::rotation_debt::encode(&debt).unwrap(),
            )
            .unwrap();
            let policy = RetentionPolicy {
                artifact_sweep_budget_ms: 5000,
                ..RetentionPolicy::default()
            };
            let (outcome, entries) = broker
                .sweep_artifacts_autonomously_with_deadlines(
                    &policy,
                    ArtifactSweepScope::Drain,
                    None,
                    None,
                    None,
                    ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET,
                )
                .unwrap();
            assert!(
                !outcome.complete,
                "consumer coverage remains unproven even when bound={bound}"
            );
            assert!(
                entries > 0,
                "independent witnessed recovery must progress when bound={bound}"
            );
            assert!(!store.join("old-cache").exists());
            assert_eq!(
                std::fs::read(protected.join("payload")).unwrap(),
                b"live in another repository"
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn closed_artifact_sweep_fixture() -> (
        tempfile::TempDir,
        crate::test_storage::TestStorage,
        Broker,
        PathBuf,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        std::fs::write(
            root.join(".gitignore"),
            "/.aethyme/\n/target/\n/ignored-data/\n/ignored-root.txt\n",
        )
        .unwrap();
        git(&root, &["add", "README.md", ".gitignore"]);
        git(&root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nauto_remove = false\nstrict = false\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 1\nclosed_worktree_grace_hours = 0\nauto_cleanup_worktrees_on_finish = false\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&root.canonicalize().unwrap()).unwrap();
        (temporary, storage, broker, root.canonicalize().unwrap())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn closed_artifact_cache(broker: &mut Broker, name: &str) -> (i64, PathBuf, PathBuf) {
        let session = broker.start_worktree(name, None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        broker
            .close_abandoning_unpushed(session.id, "closed artifact sweep fixture")
            .unwrap();
        let alias_snapshot = broker.refresh_cleanup_cohort_aliases(None).unwrap();
        assert!(
            alias_snapshot.is_some_and(|state| {
                state.complete && state.pending_dirty_rows == 0 && state.unresolved_rows == 0
            }),
            "fixture must finish the bounded closed-checkout alias inventory before the sweep"
        );
        let cache = worktree.join("target");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
        std::fs::write(cache.join("payload"), b"fixture cache").unwrap();
        (session.id, worktree, cache)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn enable_closed_artifact_sweep(root: &Path) {
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 1\nclosed_worktree_grace_hours = 0\nauto_cleanup_worktrees_on_finish = false\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn enable_ignored_directory_sweep(root: &Path) {
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nauto_remove = false\nstrict = false\nignored_directory_sweep = true\n",
        )
        .unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn set_closed_worktree_grace(root: &Path, hours: u32) {
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            format!(
                "[retention]\nclosed_worktrees_days = 1\nclosed_worktree_grace_hours = {hours}\nauto_cleanup_worktrees_on_finish = false\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\nartifact_sweep_interval_hours = 1\n"
            ),
        )
        .unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn unknown_ignored_directory(worktree: &Path) -> (PathBuf, PathBuf) {
        let ignored_directory = worktree.join("ignored-data");
        std::fs::create_dir_all(&ignored_directory).unwrap();
        std::fs::write(
            ignored_directory.join("unique-dataset.sqlite"),
            b"opaque ignored payload",
        )
        .unwrap();
        let standalone_ignored_file = worktree.join("ignored-root.txt");
        std::fs::write(&standalone_ignored_file, b"standalone ignored file").unwrap();
        (ignored_directory, standalone_ignored_file)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn managed_test_artifact_producer(
        primary_root: &Path,
        checkout_root: &Path,
        session_id: i64,
        invocation: &str,
    ) -> crate::file_lock::artifact_use::ArtifactUseProducer {
        match crate::file_lock::artifact_use::begin_artifact_use(
            primary_root,
            checkout_root,
            session_id,
            invocation,
        )
        .unwrap()
        {
            crate::file_lock::artifact_use::ArtifactUseAdmission::Admitted(producer) => *producer,
            crate::file_lock::artifact_use::ArtifactUseAdmission::ProtocolUnavailable => {
                panic!("Unix artifact-use admission should be available in this fixture")
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn run_closed_artifact_sweep(
        broker: &mut Broker,
        root: &Path,
        session_id: i64,
    ) -> (bool, usize) {
        let policy = load_retention_policy(root).unwrap();
        let (outcome, _) = broker
            .sweep_artifacts_autonomously_with_deadlines(
                &policy,
                ArtifactSweepScope::ClosedSession(session_id),
                None,
                None,
                None,
                ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET,
            )
            .unwrap();
        (outcome.complete, outcome.directories_reclaimed)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn closed_artifact_sweep_defers_while_managed_producer_is_admitted() {
        let (_temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, cache) = closed_artifact_cache(&mut broker, "producer busy");
        enable_closed_artifact_sweep(&root);
        let mut producer =
            managed_test_artifact_producer(&root, &worktree, session_id, "gc-test:producer-busy");

        let (complete_while_admitted, removed_while_admitted) =
            run_closed_artifact_sweep(&mut broker, &root, session_id);
        assert!(
            !complete_while_admitted,
            "a contended producer key is incomplete"
        );
        assert_eq!(removed_while_admitted, 0);
        assert!(
            cache.is_dir(),
            "the managed producer's cache remains intact"
        );
        assert!(
            decode_interrupted_paths(
                broker
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                    .unwrap()
                    .as_deref()
            )
            .unwrap()
            .is_empty(),
            "admission must be acquired before publishing a new removal intent"
        );

        producer.finish_not_started().unwrap();
        drop(producer);
        let (complete_after_settlement, removed_after_settlement) =
            run_closed_artifact_sweep(&mut broker, &root, session_id);
        assert!(complete_after_settlement);
        assert_eq!(removed_after_settlement, 1);
        assert!(!cache.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn closed_artifact_sweep_retains_a_dropped_unknown_producer_marker() {
        let (_temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, cache) = closed_artifact_cache(&mut broker, "unknown producer");
        enable_closed_artifact_sweep(&root);
        let producer = managed_test_artifact_producer(
            &root,
            &worktree,
            session_id,
            "gc-test:unknown-producer",
        );
        // Simulate broker death: Drop releases the advisory lock but deliberately
        // leaves the durable pre-dispatch marker unknown to future sweepers.
        drop(producer);

        let (complete, removed) = run_closed_artifact_sweep(&mut broker, &root, session_id);
        assert!(
            !complete,
            "an unknown producer marker keeps the pass incomplete"
        );
        assert_eq!(removed, 0);
        assert!(cache.is_dir());
        let blocked = broker
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_BLOCKED_PATHS_KEY)
            .unwrap()
            .expect("the unknown marker is recorded as a bounded blocker");
        assert!(blocked.contains("artifact_use_unknown_producer"));
        assert!(
            decode_interrupted_paths(
                broker
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                    .unwrap()
                    .as_deref()
            )
            .unwrap()
            .is_empty(),
            "an unknown marker must not publish a false removal intent"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn closed_artifact_sweep_advances_to_an_unrelated_checkout_when_one_key_is_busy() {
        let (_temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (busy_id, busy_worktree, busy_cache) =
            closed_artifact_cache(&mut broker, "busy closed checkout");
        let (free_id, _free_worktree, free_cache) =
            closed_artifact_cache(&mut broker, "free closed checkout");
        enable_closed_artifact_sweep(&root);
        let mut producer = managed_test_artifact_producer(
            &root,
            &busy_worktree,
            busy_id,
            "gc-test:unrelated-checkout",
        );

        let policy = load_retention_policy(&root).unwrap();
        let (outcome, _) = broker
            .sweep_artifacts_autonomously_with_deadlines(
                &policy,
                ArtifactSweepScope::Drain,
                None,
                None,
                None,
                ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET,
            )
            .unwrap();
        assert!(!outcome.complete, "the busy checkout remains a blocker");
        assert!(busy_cache.is_dir());
        assert!(
            !free_cache.exists(),
            "the unrelated checkout still progresses"
        );

        producer.finish_not_started().unwrap();
        drop(producer);
        assert_eq!(
            broker.store().session(free_id).unwrap().status,
            crate::SessionStatus::Closed
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn closed_artifact_sweep_rechecks_admission_before_resuming_interrupted_removal() {
        let (_temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, cache) = closed_artifact_cache(&mut broker, "interrupted cache");
        enable_closed_artifact_sweep(&root);
        let cache_key = cache.canonicalize().unwrap().to_string_lossy().into_owned();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_INTERRUPTED_KEY, &cache_key)
            .unwrap();
        let mut producer = managed_test_artifact_producer(
            &root,
            &worktree,
            session_id,
            "gc-test:interrupted-resume",
        );

        let (complete_while_admitted, removed_while_admitted) =
            run_closed_artifact_sweep(&mut broker, &root, session_id);
        assert!(!complete_while_admitted);
        assert_eq!(removed_while_admitted, 0);
        assert!(cache.is_dir());
        assert_eq!(
            decode_interrupted_paths(
                broker
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                    .unwrap()
                    .as_deref()
            )
            .unwrap(),
            vec![cache_key.clone()],
            "a busy resumed path keeps its durable removal intent"
        );

        producer.finish_not_started().unwrap();
        drop(producer);
        let (complete_after_settlement, removed_after_settlement) =
            run_closed_artifact_sweep(&mut broker, &root, session_id);
        assert!(complete_after_settlement);
        assert_eq!(removed_after_settlement, 1);
        assert!(!cache.exists());
        assert!(
            decode_interrupted_paths(
                broker
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                    .unwrap()
                    .as_deref()
            )
            .unwrap()
            .is_empty()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn closed_artifact_sweep_completes_missing_paths_but_retains_replaced_checkouts() {
        let (temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (missing_id, _missing_worktree, missing_cache) =
            closed_artifact_cache(&mut broker, "missing cache candidate");
        let (replaced_id, replaced_worktree, replaced_cache) =
            closed_artifact_cache(&mut broker, "replaced checkout");
        enable_closed_artifact_sweep(&root);

        let missing_key = missing_cache
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let replaced_key = replaced_cache
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let queued =
            encode_interrupted_paths(&[missing_key.clone(), replaced_key.clone()]).unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_INTERRUPTED_KEY, &queued)
            .unwrap();
        std::fs::remove_dir_all(&missing_cache).unwrap();

        let moved_checkout = temporary.path().join("moved-replaced-checkout");
        std::fs::rename(&replaced_worktree, &moved_checkout).unwrap();
        let replacement = temporary.path().join("replacement-checkout");
        let replacement_cache = replacement.join("target");
        std::fs::create_dir_all(&replacement_cache).unwrap();
        std::fs::write(replacement_cache.join("payload"), b"preserve replacement").unwrap();
        std::os::unix::fs::symlink(&replacement, &replaced_worktree).unwrap();

        let policy = load_retention_policy(&root).unwrap();
        let (outcome, _) = broker
            .sweep_artifacts_autonomously_with_deadlines(
                &policy,
                ArtifactSweepScope::Drain,
                None,
                None,
                None,
                ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET,
            )
            .unwrap();
        assert!(
            !outcome.complete,
            "a present replacement root remains blocked"
        );
        assert_eq!(
            broker.store().session(missing_id).unwrap().status,
            crate::SessionStatus::Closed
        );
        assert_eq!(
            broker.store().session(replaced_id).unwrap().status,
            crate::SessionStatus::Closed
        );
        assert!(replaced_worktree.is_symlink());
        assert_eq!(
            std::fs::read(replacement_cache.join("payload")).unwrap(),
            b"preserve replacement"
        );
        assert_eq!(
            decode_interrupted_paths(
                broker
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                    .unwrap()
                    .as_deref()
            )
            .unwrap(),
            vec![replaced_key],
            "only the proven-absent candidate may be retired from the queue"
        );
        let blocked = broker
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_BLOCKED_PATHS_KEY)
            .unwrap()
            .expect("the replacement root is recorded as blocked");
        assert!(blocked.contains("worktree_root_replaced"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn opted_in_sweep_reclaims_opaque_ignored_roots_but_keeps_files_and_checkout_grace() {
        let (_temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, cache) = closed_artifact_cache(&mut broker, "ignored data");
        let (ignored_directory, standalone_file) = unknown_ignored_directory(&worktree);
        let protected_record = worktree.join(".aethyme/qualification/manifest.json");
        std::fs::create_dir_all(protected_record.parent().unwrap()).unwrap();
        std::fs::write(&protected_record, b"broker-owned qualification record").unwrap();
        enable_closed_artifact_sweep(&root);
        enable_ignored_directory_sweep(&root);
        set_closed_worktree_grace(&root, 360);

        let (complete, removed) = run_closed_artifact_sweep(&mut broker, &root, session_id);

        assert!(complete);
        assert!(removed > 0);
        assert!(
            !ignored_directory.exists(),
            "the opt-in ignored folder is disposable"
        );
        assert!(
            !cache.exists(),
            "the existing artifact catalog still applies"
        );
        assert_eq!(
            std::fs::read(&standalone_file).unwrap(),
            b"standalone ignored file"
        );
        assert_eq!(
            std::fs::read(&protected_record).unwrap(),
            b"broker-owned qualification record",
            "broker qualification metadata remains protected even when Git ignores its parent"
        );
        assert!(worktree.is_dir(), "artifact cleanup preserves the checkout");
        assert_eq!(
            broker.store().session(session_id).unwrap().status,
            crate::SessionStatus::Closed
        );
        assert_eq!(
            load_retention_policy(&root)
                .unwrap()
                .closed_worktree_grace_hours,
            360
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn ignored_directory_sweep_is_off_by_default_and_strict_mode_keeps_it_off() {
        let (_temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, cache) = closed_artifact_cache(&mut broker, "opt in required");
        let (ignored_directory, standalone_file) = unknown_ignored_directory(&worktree);
        enable_closed_artifact_sweep(&root);

        assert!(
            !crate::AutoCleanupPolicy::load(&root)
                .unwrap()
                .ignored_directory_sweep
        );
        let (_, _) = run_closed_artifact_sweep(&mut broker, &root, session_id);
        assert!(ignored_directory.is_dir());
        assert!(standalone_file.is_file());
        assert!(!cache.exists());

        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nauto_remove = false\nstrict = true\nignored_directory_sweep = true\n",
        )
        .unwrap();
        let (complete, _) = run_closed_artifact_sweep(&mut broker, &root, session_id);
        assert!(!complete, "strict mode pauses unattended artifact cleanup");
        assert!(ignored_directory.is_dir());
        assert!(standalone_file.is_file());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn ignored_directory_recovery_queue_is_typed_and_legacy_paths_stay_catalog_only() {
        let path = std::env::temp_dir()
            .join("closed-checkout/ignored-data")
            .to_string_lossy()
            .into_owned();
        let encoded = encode_interrupted_ignored_directories(std::slice::from_ref(&path)).unwrap();

        assert_eq!(
            decode_interrupted_ignored_directories(Some(&encoded)).unwrap(),
            vec![path.clone()]
        );
        assert_eq!(
            decode_interrupted_paths(Some(&path)).unwrap(),
            vec![path.clone()]
        );
        assert!(decode_interrupted_ignored_directories(Some(&path)).is_err());
        assert!(decode_interrupted_ignored_directories(Some(
            r#"{"version":2,"paths":[{"kind":"ignored_directory_root","path":"/tmp/closed-checkout/ignored-data"}]}"#
        ))
        .is_err());
        assert!(decode_interrupted_ignored_directories(Some(
            r#"{"version":1,"paths":[{"kind":"future_kind","path":"/tmp/closed-checkout/ignored-data"}]}"#
        ))
        .is_err());
        assert!(decode_interrupted_ignored_directories(Some(
            r#"{"version":1,"paths":[{"kind":"ignored_directory_root","path":"relative/ignored-data"}]}"#
        ))
        .is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn ignored_directory_policy_drift_after_plan_blocks_before_unlink() {
        let (_temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, _) = closed_artifact_cache(&mut broker, "policy drift");
        let (ignored_directory, _) = unknown_ignored_directory(&worktree);
        enable_closed_artifact_sweep(&root);
        enable_ignored_directory_sweep(&root);

        let _change_policy_after_plan =
            ArtifactSweepTestMutationInterrupt::rewrite_file_before_removal(
                &ignored_directory,
                &root.join(".aethyme/config.toml"),
                "[cleanup]\nauto_remove = false\nstrict = true\nignored_directory_sweep = true\n",
            );
        let (complete, _) = run_closed_artifact_sweep(&mut broker, &root, session_id);

        assert!(!complete);
        assert!(ignored_directory.is_dir());
        assert!(ignored_directory.join("unique-dataset.sqlite").is_file());
        assert_eq!(
            decode_interrupted_ignored_directories(
                broker
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_IGNORED_DIR_INTERRUPTED_KEY)
                    .unwrap()
                    .as_deref()
            )
            .unwrap(),
            vec![ignored_directory.to_string_lossy().into_owned()],
            "the typed resume witness is retained after policy drift"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn ignored_directory_status_drift_after_plan_blocks_before_unlink() {
        let (_temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, _) = closed_artifact_cache(&mut broker, "ignore drift");
        let (ignored_directory, _) = unknown_ignored_directory(&worktree);
        enable_closed_artifact_sweep(&root);
        enable_ignored_directory_sweep(&root);

        let _remove_ignore_after_plan =
            ArtifactSweepTestMutationInterrupt::rewrite_file_before_removal(
                &ignored_directory,
                &worktree.join(".gitignore"),
                "/.aethyme/\n/target/\n/ignored-root.txt\n",
            );
        let (complete, _) = run_closed_artifact_sweep(&mut broker, &root, session_id);

        assert!(!complete);
        assert!(ignored_directory.is_dir());
        assert!(ignored_directory.join("unique-dataset.sqlite").is_file());
        assert_eq!(
            decode_interrupted_ignored_directories(
                broker
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_IGNORED_DIR_INTERRUPTED_KEY)
                    .unwrap()
                    .as_deref()
            )
            .unwrap(),
            vec![ignored_directory.to_string_lossy().into_owned()]
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn ignored_directory_partial_removal_waits_for_opt_in_before_resuming() {
        let (_temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, cache) =
            closed_artifact_cache(&mut broker, "partial ignored data");
        std::fs::remove_dir_all(cache).unwrap();
        let ignored_directory = worktree.join("ignored-data");
        std::fs::create_dir_all(&ignored_directory).unwrap();
        std::fs::write(ignored_directory.join("first"), b"first").unwrap();
        std::fs::write(ignored_directory.join("second"), b"second").unwrap();
        enable_closed_artifact_sweep(&root);
        enable_ignored_directory_sweep(&root);

        let _interrupt_after_first_unlink =
            ArtifactSweepTestMutationInterrupt::after_first_unlink(&ignored_directory);
        let (partial, removed) = run_closed_artifact_sweep(&mut broker, &root, session_id);
        assert!(!partial);
        assert_eq!(removed, 0, "a partially removed directory is not reclaimed");
        assert!(ignored_directory.is_dir());
        assert_eq!(
            std::fs::read_dir(&ignored_directory).unwrap().count(),
            1,
            "exactly one payload was removed before interruption"
        );
        let typed_queue = broker
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_IGNORED_DIR_INTERRUPTED_KEY)
            .unwrap()
            .expect("partial generic removal persists its typed path witness");
        assert_eq!(
            decode_interrupted_ignored_directories(Some(&typed_queue)).unwrap(),
            vec![ignored_directory.to_string_lossy().into_owned()]
        );
        drop(_interrupt_after_first_unlink);

        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nauto_remove = false\nstrict = false\nignored_directory_sweep = false\n",
        )
        .unwrap();
        let (disabled_complete, _) = run_closed_artifact_sweep(&mut broker, &root, session_id);
        assert!(!disabled_complete);
        assert!(ignored_directory.is_dir());
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_IGNORED_DIR_INTERRUPTED_KEY)
                .unwrap()
                .as_deref(),
            Some(typed_queue.as_str()),
            "opt-out does not weaken the persisted witness or continue deletion"
        );

        enable_ignored_directory_sweep(&root);
        let (complete, _) = run_closed_artifact_sweep(&mut broker, &root, session_id);
        assert!(complete);
        assert!(!ignored_directory.exists());
        assert!(
            decode_interrupted_ignored_directories(
                broker
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_IGNORED_DIR_INTERRUPTED_KEY)
                    .unwrap()
                    .as_deref()
            )
            .unwrap()
            .is_empty()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn ignored_directory_replacement_with_symlink_is_retained() {
        let (temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, _) = closed_artifact_cache(&mut broker, "symlink replacement");
        let (ignored_directory, _) = unknown_ignored_directory(&worktree);
        let moved_directory = temporary.path().join("moved-ignored-directory");
        let replacement = temporary.path().join("replacement-target");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(replacement.join("keep"), b"outside target").unwrap();
        enable_closed_artifact_sweep(&root);
        enable_ignored_directory_sweep(&root);

        let _replace_after_plan =
            ArtifactSweepTestMutationInterrupt::replace_directory_with_symlink(
                &ignored_directory,
                &ignored_directory,
                &moved_directory,
                &replacement,
            );
        let (complete, _) = run_closed_artifact_sweep(&mut broker, &root, session_id);

        assert!(!complete);
        assert!(ignored_directory.is_symlink());
        assert!(moved_directory.join("unique-dataset.sqlite").is_file());
        assert_eq!(
            std::fs::read(replacement.join("keep")).unwrap(),
            b"outside target"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn replaced_checkout_root_after_plan_is_retained() {
        let (temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, _) = closed_artifact_cache(&mut broker, "checkout root drift");
        let (ignored_directory, _) = unknown_ignored_directory(&worktree);
        let moved_checkout = temporary.path().join("moved-checkout-root");
        let replacement = temporary.path().join("replacement-checkout-root");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::create_dir(replacement.join("ignored-data")).unwrap();
        std::fs::write(
            replacement.join("ignored-data/keep"),
            b"replacement checkout contents",
        )
        .unwrap();
        enable_closed_artifact_sweep(&root);
        enable_ignored_directory_sweep(&root);

        let _replace_checkout_after_plan =
            ArtifactSweepTestMutationInterrupt::replace_directory_with_symlink(
                &ignored_directory,
                &worktree,
                &moved_checkout,
                &replacement,
            );
        let (complete, _) = run_closed_artifact_sweep(&mut broker, &root, session_id);

        assert!(!complete);
        assert!(worktree.is_symlink());
        assert!(
            moved_checkout
                .join("ignored-data/unique-dataset.sqlite")
                .is_file()
        );
        assert_eq!(
            std::fs::read(replacement.join("ignored-data/keep")).unwrap(),
            b"replacement checkout contents"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn live_checkout_alias_blocks_ignored_directory_reclamation() {
        use std::os::unix::fs::symlink;

        let (temporary, _storage, mut broker, root) = closed_artifact_sweep_fixture();
        let (session_id, worktree, _) = closed_artifact_cache(&mut broker, "live alias");
        let (ignored_directory, _) = unknown_ignored_directory(&worktree);
        let live_alias = temporary.path().join("live-checkout-alias");
        symlink(&worktree, &live_alias).unwrap();
        broker
            .store()
            .register_session(&crate::types::NewSession {
                worktree_path: live_alias.to_string_lossy().into_owned(),
                branch: "live-checkout-alias".into(),
                origin: crate::types::SessionOrigin::Adopted,
                task: Some("live checkout alias blocker".into()),
                diff_base: None,
                adoption_base: None,
                adopted_head: None,
                repository_contract: None,
                pid: None,
                command: None,
                log_path: None,
                agent_identity: None,
            })
            .unwrap();
        enable_closed_artifact_sweep(&root);
        enable_ignored_directory_sweep(&root);

        let (complete, _) = run_closed_artifact_sweep(&mut broker, &root, session_id);

        assert!(!complete);
        assert!(ignored_directory.is_dir());
        assert!(ignored_directory.join("unique-dataset.sqlite").is_file());
    }

    #[test]
    fn ignored_directory_policy_honors_keep_globs() {
        let policy = crate::AutoCleanupPolicy {
            ignored_directory_sweep: true,
            keep: vec!["*-pinned".into()],
            ..Default::default()
        };

        assert!(!ignored_directory_sweep_policy_allows_checkout(
            &policy,
            Path::new("/worktrees/agent-pinned")
        ));
        assert!(ignored_directory_sweep_policy_allows_checkout(
            &policy,
            Path::new("/worktrees/agent-disposable")
        ));
    }

    fn repository_with_pending_gc_journal() -> (
        tempfile::TempDir,
        PathBuf,
        crate::test_storage::TestStorage,
        PathBuf,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nartifact_sweep_budget_ms = 0\nstartup_budget_ms = 5000\n",
        )
        .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let plan = broker.gc_plan().unwrap();
        assert_eq!(plan.digest.len(), 64, "fixture GC plan must be complete");
        let journal_path = root.join(".aethyme/gc-journal.json");
        // Model a crash after a complete operator-reviewed plan was durably
        // published, before its first apply step.
        write_journal(&journal_path, &GcJournal::from(plan)).unwrap();
        drop(broker);
        (temporary, root, storage, journal_path)
    }

    #[test]
    fn cleanup_resolve_open_resumes_a_confirmed_gc_journal_without_starting_maintenance() {
        let (_temporary, root, storage, journal_path) = repository_with_pending_gc_journal();
        ARTIFACT_WORKER_ADMISSION_ATTEMPTS.with(|attempts| attempts.borrow_mut().clear());

        let _broker = Broker::open_for_cleanup_resolve_with_options(&root, storage.options())
            .expect("cleanup resolve keeps the ordinary writable broker open");

        assert!(
            !journal_path.exists(),
            "a confirmed GC journal must still resume during cleanup-resolve open"
        );
        assert_eq!(
            ARTIFACT_WORKER_ADMISSION_ATTEMPTS.with(|attempts| attempts.borrow().clone()),
            Vec::<bool>::new(),
            "cleanup-resolve open must not admit the unrelated artifact-sweep worker"
        );
    }

    #[test]
    fn normal_open_still_resumes_confirmed_gc_journals_and_runs_startup_maintenance() {
        let (_temporary, root, storage, journal_path) = repository_with_pending_gc_journal();
        ARTIFACT_WORKER_ADMISSION_ATTEMPTS.with(|attempts| attempts.borrow_mut().clear());

        let _broker = storage
            .open(&root)
            .expect("normal writable open retains its startup maintenance");

        assert!(
            !journal_path.exists(),
            "normal open must continue resuming confirmed GC journals"
        );
        assert_eq!(
            ARTIFACT_WORKER_ADMISSION_ATTEMPTS.with(|attempts| attempts.borrow().clone()),
            vec![false],
            "normal open must retain its artifact-sweep worker admission"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn broker_open_keeps_published_rotation_debt_pending_when_sweep_is_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nartifact_sweep_budget_ms = 0\n",
        )
        .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        // Startup maintenance is deferred until this repository has an
        // identity-bound worktree root. Create the binding through the real
        // managed lifecycle before publishing the debt that the reopen must
        // recover.
        let _binding_session = broker
            .start_worktree("rotation debt fixture", None)
            .unwrap();
        let cache_root = broker
            .storage_context()
            .preparation_cache_root()
            .expect("test storage has a captured preparation-cache root")
            .expect("captured cache root exists");
        std::fs::create_dir_all(&cache_root).unwrap();
        let repository = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let store = cache_root.join(repository);
        std::fs::create_dir(&store).unwrap();
        std::fs::write(store.join("payload"), b"preserve until verified recovery").unwrap();
        let root_identity =
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&cache_root).unwrap();
        let debt = crate::preparation::rotation_debt::RotationDebt::new(
            repository,
            &crate::preparation::rotation_debt::new_transaction_id(),
            root_identity.clone(),
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&store).unwrap(),
        )
        .unwrap();
        let resource_db_path = broker
            .storage_context()
            .host_resource_db_path
            .clone()
            .expect("the fixture captures its host-resource database");
        let payload = crate::preparation::rotation_debt::encode(&debt).unwrap();
        crate::preparation::rotation_journal::publish(
            &resource_db_path,
            &cache_root,
            &root_identity.encode().unwrap(),
            repository,
            &payload,
        )
        .unwrap();
        crate::preparation::rotation_debt::publish(&cache_root, debt).unwrap();
        let retained = crate::preparation::rotation_debt::read(&cache_root, repository)
            .unwrap()
            .expect("the witness is published before the cache is retired");
        crate::preparation::rotation_debt::retire_original_store(&cache_root, &retained).unwrap();
        let retired = cache_root.join(format!("{repository}.retired"));
        drop(broker);

        // This is a normal startup path. No explicit maintenance call repairs
        // the state: Broker::open must notice the published witness itself.
        let mut reopened = storage.open(&root).unwrap();
        let requested = reopened
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
            .unwrap()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = reopened
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
            .unwrap()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        assert!(
            requested > completed,
            "the recovered request remains pending"
        );
        assert!(
            crate::preparation::rotation_debt::read(&cache_root, repository)
                .unwrap()
                .is_some(),
            "zero-budget startup must retain the durable debt witness"
        );
        assert_eq!(
            std::fs::read(retired.join("payload")).unwrap(),
            b"preserve until verified recovery",
            "zero-budget startup must not delete the old cache store"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn an_open_broker_keeps_recovery_pending_when_explicit_cache_parent_disappears() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nartifact_sweep_budget_ms = 5000\n",
        )
        .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let cache_root_override = storage.root().join("cache-owner").join("preparation-cache");
        let cache_parent = cache_root_override.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&cache_root_override).unwrap();
        let options = storage
            .options()
            .with_preparation_cache_root(cache_root_override.clone());
        let mut first = Broker::open_with_options(&root, options).unwrap();
        let cache_namespace = first
            .storage_context()
            .preparation_cache_root()
            .expect("test storage has a captured preparation-cache root")
            .expect("captured cache root exists");
        assert_eq!(
            cache_namespace.parent(),
            Some(cache_root_override.as_path())
        );
        std::fs::create_dir_all(&cache_namespace).unwrap();
        for key in [
            ARTIFACT_SWEEP_STAMP_KEY,
            ARTIFACT_SWEEP_PROGRESS_KEY,
            ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY,
            ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY,
            ARTIFACT_SWEEP_RETRY_AFTER_KEY,
        ] {
            first.store().meta_set(key, "").unwrap();
        }
        let namespace = first.ensure_artifact_sweep_namespace().unwrap();
        let requested_key =
            scoped_artifact_sweep_key(&namespace, ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY);
        let completed_key =
            scoped_artifact_sweep_key(&namespace, ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY);
        first
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "7")
            .unwrap();
        first
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "3")
            .unwrap();
        std::fs::remove_dir_all(&cache_parent).unwrap();

        first.resume_gc_maintenance().unwrap();
        let requested = first
            .store()
            .meta_get(&requested_key)
            .unwrap()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let completed = first
            .store()
            .meta_get(&completed_key)
            .unwrap()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        assert!(
            requested > completed,
            "unknown cache location must leave its durable recovery request pending"
        );
        assert!(
            !cache_parent.exists(),
            "maintenance must not recreate a missing cache-owner directory"
        );
        assert!(
            storage.root().is_dir(),
            "removing the cache owner must not remove the TestStorage owner"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn fresh_open_refuses_a_missing_explicit_cache_root_without_recreating_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let cache_root_override = storage.root().join("cache-owner").join("preparation-cache");
        let cache_parent = cache_root_override.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&cache_root_override).unwrap();
        let options = storage
            .options()
            .with_preparation_cache_root(cache_root_override.clone());
        let first = Broker::open_with_options(&root, options).unwrap();
        let cache_namespace = first
            .storage_context()
            .preparation_cache_root()
            .expect("test storage has a captured preparation-cache root")
            .expect("captured cache root exists");
        assert_eq!(
            cache_namespace.parent(),
            Some(cache_root_override.as_path())
        );
        drop(first);
        std::fs::remove_dir_all(&cache_parent).unwrap();

        let error = match Broker::open_with_options(
            &root,
            storage
                .options()
                .with_preparation_cache_root(cache_root_override.clone()),
        ) {
            Ok(_) => panic!("a fresh open must refuse a missing explicit cache root"),
            Err(error) => error,
        };
        assert!(
            matches!(&error, crate::BrokerOpError::InvalidStorageContext { .. }),
            "fresh open returned an unexpected error: {error:?}"
        );
        assert!(
            !cache_root_override.exists(),
            "fresh open must not recreate a missing explicit cache root"
        );
        assert!(
            storage.root().is_dir(),
            "removing the cache owner must not remove the TestStorage owner"
        );
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
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(tmp.path()).unwrap();
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
                None,
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
        assert!(!scope_runs_checkout_cleanup(
            ArtifactSweepScope::FinishDrain
        ));
        assert!(scope_runs_checkout_cleanup(ArtifactSweepScope::Drain));
        assert!(!scope_runs_checkout_cleanup(
            ArtifactSweepScope::ClosedSession(1)
        ));
    }

    #[test]
    fn foreground_sweep_reserves_checkout_time_inside_the_existing_budget() {
        let started_at = Instant::now();
        let budget_ms = 60_000;
        let reserve =
            foreground_checkout_cleanup_reserve(budget_ms, ArtifactSweepScope::Drain, true);
        let deadlines = artifact_sweep_deadlines(started_at, budget_ms, reserve, None, None, None);

        assert_eq!(reserve, Duration::from_secs(30));
        assert_eq!(
            deadlines.discovery,
            started_at + Duration::from_secs(30),
            "cache discovery uses only the first part of the sweep budget"
        );
        assert_eq!(deadlines.removal, deadlines.discovery);
        assert_eq!(
            deadlines.checkout_cleanup,
            started_at + Duration::from_secs(60),
            "checkout cleanup retains the budget's original end"
        );
        assert_eq!(
            deadlines
                .checkout_cleanup
                .duration_since(deadlines.discovery),
            reserve,
            "the cleanup tranche is reserved inside, not added to, the command budget"
        );

        assert_eq!(
            foreground_checkout_cleanup_reserve(100, ArtifactSweepScope::Drain, true),
            Duration::from_millis(50),
            "a small pass splits its budget without exceeding half"
        );
        assert_eq!(
            foreground_checkout_cleanup_reserve(60_000, ArtifactSweepScope::Drain, false),
            Duration::ZERO,
            "disabled checkout cleanup does not reduce cache time"
        );
        assert_eq!(
            foreground_checkout_cleanup_reserve(
                60_000,
                ArtifactSweepScope::AutomaticContinuation,
                true
            ),
            Duration::ZERO,
            "the cache-only continuation has no foreground cleanup tranche"
        );

        let override_deadline = started_at + Duration::from_secs(1);
        let overridden = artifact_sweep_deadlines(
            started_at,
            budget_ms,
            reserve,
            Some(override_deadline),
            None,
            None,
        );
        assert_eq!(
            overridden.checkout_cleanup, override_deadline,
            "the existing test deadline override still caps both lanes"
        );
        let cache_deadline = started_at - Duration::from_millis(1);
        let later_cleanup_deadline = started_at + Duration::from_secs(30);
        let split = artifact_sweep_deadlines(
            started_at,
            budget_ms,
            reserve,
            Some(cache_deadline),
            Some(cache_deadline),
            Some(later_cleanup_deadline),
        );
        assert_eq!(split.discovery, cache_deadline);
        assert_eq!(split.removal, cache_deadline);
        assert_eq!(split.checkout_cleanup, later_cleanup_deadline);
    }

    #[cfg(unix)]
    #[test]
    fn expired_cache_deadlines_still_dispatch_checkout_cleanup() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", ".gitignore", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        git(&root, &["remote", "add", "origin", "."]);
        git(
            &root,
            &["fetch", "-q", "origin", "main:refs/remotes/origin/main"],
        );
        git(&root, &["remote", "set-head", "origin", "main"]);

        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nauto_remove = true\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 1\nclosed_worktree_grace_hours = 0\nauto_cleanup_worktrees_on_finish = false\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let cache_owner = broker
            .start_worktree("unrepresented cache owner", None)
            .unwrap();
        let cache_owner_worktree = PathBuf::from(&cache_owner.worktree_path);
        std::fs::write(cache_owner_worktree.join("unrepresented.txt"), "local\n").unwrap();
        git(&cache_owner_worktree, &["add", "unrepresented.txt"]);
        git(&cache_owner_worktree, &["commit", "-qm", "unrepresented"]);
        broker
            .close_abandoning_unpushed(cache_owner.id, "fixture cache owner")
            .unwrap();
        assert!(
            broker
                .store()
                .active_leases()
                .unwrap()
                .iter()
                .all(|lease| lease.session_id != cache_owner.id),
            "the closed fixture owner must not retain active leases"
        );

        let cache = cache_owner_worktree.join("target");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
        std::fs::write(cache.join("payload"), b"unreached cache backlog").unwrap();

        let disposable = broker
            .start_worktree("eligible closed checkout", None)
            .unwrap();
        let disposable_worktree = PathBuf::from(&disposable.worktree_path);
        broker.close(disposable.id).unwrap();

        let stale_open = broker
            .start_worktree("stale session retirement candidate", None)
            .unwrap();
        age_open_session_for_retirement(
            &mut broker,
            stale_open.id,
            Path::new(&stale_open.worktree_path),
        );

        let now = Instant::now();
        let cache_deadline = now - Duration::from_millis(1);
        let checkout_cleanup_deadline = now + Duration::from_secs(30);
        let policy = load_retention_policy(&root).unwrap();
        let (outcome, _) = broker
            .sweep_artifacts_autonomously_with_test_lane_deadlines(
                &policy,
                ArtifactSweepScope::Drain,
                cache_deadline,
                cache_deadline,
                checkout_cleanup_deadline,
            )
            .unwrap();

        assert!(!outcome.complete, "expired cache discovery is incomplete");
        assert_eq!(outcome.directories_reclaimed, 0);
        assert!(
            cache.join("payload").is_file(),
            "the cache lane stays stopped"
        );
        let cleanup = outcome
            .auto_cleanup
            .expect("the independent checkout lane still runs");
        assert_eq!(
            cleanup.sessions_retired, 1,
            "an expired cache deadline cannot consume the retirement tranche"
        );
        assert_eq!(
            broker.store().session(stale_open.id).unwrap().cleanup_state,
            crate::SessionCleanupState::Closed
        );
        assert!(cleanup.removed.iter().any(|removed| {
            removed.sessions.contains(&disposable.id)
                && removed.worktree == disposable_worktree.to_string_lossy().as_ref()
        }));
        assert!(
            !disposable_worktree.exists(),
            "the eligible clean checkout is removed"
        );
        assert!(
            cache_owner_worktree.join("unrepresented.txt").is_file(),
            "the owner with an unrepresented commit remains protected"
        );
        assert!(
            cache.join("payload").is_file(),
            "checkout cleanup does not consume the unscanned cache"
        );
    }

    #[test]
    fn finish_drains_artifacts_without_running_whole_checkout_cleanup() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", ".gitignore", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        git(&root, &["remote", "add", "origin", "."]);
        git(
            &root,
            &["fetch", "-q", "origin", "main:refs/remotes/origin/main"],
        );
        git(&root, &["remote", "set-head", "origin", "main"]);
        git(&root, &["rev-parse", "refs/remotes/origin/HEAD"]);

        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nauto_remove = true\n",
        )
        .unwrap();
        // Use the minimum valid retention age; the later origin-containment
        // proof makes this just-closed checkout eligible immediately.
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 1\nclosed_worktree_grace_hours = 0\nauto_cleanup_worktrees_on_finish = false\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let abandoned = broker
            .start_worktree("unrepresented artifact fixture", None)
            .unwrap();
        let abandoned_worktree = PathBuf::from(&abandoned.worktree_path);
        std::fs::write(abandoned_worktree.join("unrepresented.txt"), "local\n").unwrap();
        git(&abandoned_worktree, &["add", "unrepresented.txt"]);
        git(&abandoned_worktree, &["commit", "-qm", "unrepresented"]);
        // This deliberately closed cache owner has an unrepresented commit,
        // so it must stay out of whole-checkout cleanup. The public finish
        // path rejects that state before its --abandon-unpushed check; use the
        // low-level close API to create the closed owner this sweep fixture
        // needs. No explicit paths were leased, and low-level close leaves
        // active leases in place, so assert this fixture is lease-free.
        broker
            .close_abandoning_unpushed(abandoned.id, "fixture backlog owner")
            .unwrap();
        let abandoned_status = broker.store().session(abandoned.id).unwrap().status;
        assert_eq!(
            abandoned_status,
            crate::SessionStatus::Closed,
            "the backlog owner must already be closed before its cache is seeded"
        );
        assert!(
            abandoned_worktree.join("README.md").is_file(),
            "low-level close must retain the checkout for this fixture"
        );
        assert!(
            broker
                .store()
                .active_leases()
                .unwrap()
                .iter()
                .all(|lease| lease.session_id != abandoned.id),
            "the fixture must not leave active leases on a low-level-closed session"
        );

        let cache = abandoned_worktree.join("target");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
        std::fs::write(cache.join("payload"), b"interrupted artifact backlog").unwrap();
        let cache_key = cache.canonicalize().unwrap().to_string_lossy().into_owned();

        let current = broker
            .start_worktree("retained finish fixture", None)
            .unwrap();
        let current_worktree = PathBuf::from(&current.worktree_path);
        git(
            &current_worktree,
            &[
                "merge-base",
                "--is-ancestor",
                "HEAD",
                "refs/remotes/origin/main",
            ],
        );
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_INTERRUPTED_KEY, &cache_key)
            .unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_PROGRESS_KEY, "1")
            .unwrap();
        let queued_paths = broker
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
            .unwrap();
        assert_eq!(
            decode_interrupted_paths(queued_paths.as_deref()).unwrap(),
            vec![cache_key.clone()]
        );
        assert!(cache.join("CACHEDIR.TAG").is_file());
        std::fs::remove_file(cache.join("CACHEDIR.TAG")).unwrap();
        assert!(!cache.join("CACHEDIR.TAG").exists());
        assert!(cache.join("payload").is_file());
        assert!(broker.artifact_sweep_backlog_pending().unwrap());

        let report = broker
            .finish_with_options(
                current.id,
                crate::FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap();
        assert!(
            report.closed,
            "finish should close the represented current session: status={:?}, unsubmitted_commits={}, warnings={:?}, next_commands={:?}",
            report.status, report.unsubmitted_commits, report.warnings, report.next_commands
        );
        assert!(
            report.cleanup.kept,
            "finish should keep the current checkout: status={:?}, warnings={:?}",
            report.status, report.warnings
        );
        assert!(
            !report.cleanup.worktree_removed,
            "finish removed the current checkout: status={:?}, warnings={:?}",
            report.status, report.warnings
        );
        assert!(current_worktree.join("README.md").is_file());
        assert!(
            abandoned_worktree.join("README.md").is_file(),
            "finish must not clean up the whole unrepresented cache-owner checkout"
        );
        assert!(
            abandoned_worktree.join("unrepresented.txt").is_file(),
            "finish must preserve the unrepresented commit that excludes this checkout from cleanup"
        );
        assert!(
            !cache.exists(),
            "finish should drain the pending cache backlog"
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("continued the build-artifact sweep"))
        );
        assert!(
            decode_interrupted_paths(
                broker
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                    .unwrap()
                    .as_deref()
            )
            .unwrap()
            .is_empty()
        );
        let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
        let selected = plan.selected_worktrees();
        assert!(
            selected.contains(&current_worktree.to_string_lossy().into_owned()),
            "the retained checkout must remain eligible under explicit auto_remove=true"
        );
        assert!(
            !selected.contains(&abandoned_worktree.to_string_lossy().into_owned()),
            "the unrepresented cache owner must remain ineligible for whole-checkout cleanup"
        );
    }

    #[test]
    fn strict_policy_blocks_real_autonomous_artifact_sweep_until_disabled() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", ".gitignore", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        git(&root, &["remote", "add", "origin", "."]);
        git(
            &root,
            &["fetch", "-q", "origin", "main:refs/remotes/origin/main"],
        );
        git(&root, &["remote", "set-head", "origin", "main"]);
        git(&root, &["rev-parse", "refs/remotes/origin/HEAD"]);

        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nstrict = true\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 30\nclosed_worktree_grace_hours = 0\nauto_cleanup_worktrees_on_finish = false\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let abandoned = broker
            .start_worktree("strict artifact sweep fixture", None)
            .unwrap();
        let worktree = PathBuf::from(&abandoned.worktree_path);
        std::fs::write(worktree.join("unrepresented.txt"), "local\n").unwrap();
        git(&worktree, &["add", "unrepresented.txt"]);
        git(&worktree, &["commit", "-qm", "unrepresented"]);
        broker
            .close_abandoning_unpushed(abandoned.id, "fixture backlog owner")
            .unwrap();

        let cache = worktree.join("target");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
        std::fs::write(cache.join("payload"), b"known ignored cache").unwrap();
        assert!(cache.join("CACHEDIR.TAG").is_file());

        let disposable = broker
            .start_worktree("ignored-free closed checkout", None)
            .unwrap();
        let disposable_worktree = PathBuf::from(&disposable.worktree_path);
        broker.close(disposable.id).unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_STAMP_KEY, &now_ms().to_string())
            .unwrap();
        let policy = load_retention_policy(&root).unwrap();
        let skipped = broker
            .sweep_artifacts_autonomously(&policy, ArtifactSweepScope::Inline)
            .unwrap();
        assert!(
            skipped.auto_cleanup.is_none(),
            "strict inline mode must preserve the normal cadence"
        );
        assert!(disposable_worktree.exists());
        assert!(cache.join("payload").exists());
        let strict_report = broker.gc_sweep_artifacts().unwrap();
        assert!(
            !disposable_worktree.exists(),
            "strict mode must still remove an eligible ignored-free checkout: {strict_report:#?}"
        );
        assert!(strict_report.auto_cleanup.as_ref().is_some_and(|report| {
            report
                .removed
                .iter()
                .any(|removed| removed.sessions.contains(&disposable.id))
        }));
        assert_eq!(
            strict_report.directories_reclaimed, 0,
            "strict policy must suppress the real autonomous artifact sweep"
        );
        assert!(cache.join("payload").is_file());
        assert!(worktree.join("unrepresented.txt").is_file());
        assert!(worktree.join("README.md").is_file());

        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nstrict = false\n",
        )
        .unwrap();
        let disposable_report = broker.gc_sweep_artifacts().unwrap();
        assert_eq!(
            disposable_report.directories_reclaimed, 1,
            "the same eligible ignored target should be reclaimed after strict is disabled"
        );
        assert!(!cache.exists());
        assert!(
            worktree.join("unrepresented.txt").is_file(),
            "artifact reclamation must leave the checkout protected by its unrepresented commit"
        );
        assert!(worktree.join("README.md").is_file());
    }

    #[test]
    fn interrupted_tree_reports_entries_unlinked_before_its_cursor_stalls() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("target");
        // An expired budget may unlink one flat leaf, but must not descend
        // into an arbitrary directory chain before checking that budget.
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("first"), b"first").unwrap();
        std::fs::write(cache.join("second"), b"second").unwrap();

        let (outcome, entries_reclaimed) =
            super::tree_removal::remove_condemned_tree_with_progress(
                &cache,
                None,
                Some(Instant::now()),
            )
            .unwrap();

        assert_eq!(outcome, TreeRemoval::Interrupted);
        assert_eq!(
            entries_reclaimed, 1,
            "report the single successful unlink before the expired budget stops this pass"
        );
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
        assert!(cache.exists(), "the interrupted cache remains resumable");
    }

    #[test]
    fn inline_partial_cache_removal_respects_spacing_and_resumes_afterward() {
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
            "[retention]\nclosed_worktrees_days = 30\nclosed_worktree_grace_hours = 0\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();

        let session = broker
            .start_worktree("partial cache fixture", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("landed.txt"), "landed\n").unwrap();
        git(&worktree, &["add", "landed.txt"]);
        git(&worktree, &["commit", "-qm", "landed"]);
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
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
        std::fs::write(cache.join("first"), b"first").unwrap();
        std::fs::write(cache.join("second"), b"second").unwrap();
        let cache_key = cache.canonicalize().unwrap().to_string_lossy().into_owned();

        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 30\nclosed_worktree_grace_hours = 0\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();
        let policy = load_retention_policy(root).unwrap();
        let (partial, entries_reclaimed) = {
            let _interrupt_after_first_unlink =
                ArtifactSweepTestMutationInterrupt::after_first_unlink(&cache);
            broker
                .sweep_artifacts_autonomously_with_test_deadlines(
                    &policy,
                    ArtifactSweepScope::Inline,
                    Instant::now() + Duration::from_secs(30),
                    Instant::now() + Duration::from_secs(30),
                )
                .unwrap()
        };
        assert!(
            !partial.complete,
            "an interrupted removal is not a full lap"
        );
        assert_eq!(entries_reclaimed, 1);
        assert!(cache.join("CACHEDIR.TAG").is_file(), "the witness remains");
        let queued = decode_interrupted_paths(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                .unwrap()
                .as_deref(),
        )
        .unwrap();
        assert_eq!(queued, vec![cache_key.clone()]);
        let partial_stamp = broker
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_PROGRESS_KEY)
            .unwrap()
            .expect("the incomplete inline pass records its spacing stamp");
        assert_ne!(partial_stamp, "0");

        let entries_after_partial = std::fs::read_dir(&cache).unwrap().count();
        assert_eq!(entries_after_partial, 2);
        assert!(cache.join("first").exists() || cache.join("second").exists());
        let (_, spaced_entries) = broker
            .sweep_artifacts_autonomously_with_test_deadlines(
                &policy,
                ArtifactSweepScope::Inline,
                Instant::now() + Duration::from_secs(30),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
        assert_eq!(spaced_entries, 0, "spacing prevents an immediate retry");
        assert_eq!(
            std::fs::read_dir(&cache).unwrap().count(),
            entries_after_partial,
            "the queued cache is untouched inside the spacing interval"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                .unwrap()
                .as_deref(),
            Some(cache_key.as_str()),
            "the partial path remains queued while the retry is spaced"
        );

        // Advance only the persisted schedule marker; the per-unlink hook
        // forced the partial deletion deterministically without sleeping.
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_PROGRESS_KEY, "0")
            .unwrap();
        let (resumed, resumed_entries) = broker
            .sweep_artifacts_autonomously_with_test_deadlines(
                &policy,
                ArtifactSweepScope::Inline,
                Instant::now() + Duration::from_secs(30),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
        assert!(resumed.complete, "the eligible retry finishes the cache");
        assert!(resumed_entries > 0);
        assert!(!cache.exists());
        assert!(
            worktree.join("landed.txt").is_file(),
            "the checkout remains"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                .unwrap()
                .as_deref(),
            Some(""),
            "completion clears the durable interrupted-path queue"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_PROGRESS_KEY)
                .unwrap()
                .as_deref(),
            Some("0")
        );
    }

    #[test]
    fn an_automatic_recovery_request_advances_its_durable_generation() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
        git(tmp.path(), &["add", "README.md"]);
        git(tmp.path(), &["commit", "-qm", "init"]);
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(tmp.path()).unwrap();

        let prior_generation = broker
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
            .unwrap()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        broker.persist_artifact_sweep_recovery_request().unwrap();
        assert!(broker.artifact_sweep_recovery_requested().unwrap());
        let first = broker
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
            .unwrap();
        let first_generation = first.as_ref().unwrap().parse::<i64>().unwrap();
        assert!(
            first_generation > prior_generation,
            "a fresh request advances any generation produced during startup"
        );
        broker.persist_artifact_sweep_recovery_request().unwrap();
        let second = broker
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
            .unwrap();
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
    // The test-only launcher holds its owned process group until the parent
    // tears it down. Its grandchild worker uses the same process-group setup
    // as the installed continuation command and must survive that teardown.
    #[test]
    #[allow(clippy::zombie_processes)]
    fn detached_artifact_worker_child_fixture() {
        let Ok(mode) = std::env::var(DETACHED_FIXTURE_MODE_ENV) else {
            return;
        };
        let root = PathBuf::from(std::env::var_os(DETACHED_FIXTURE_ROOT_ENV).unwrap());
        match mode.as_str() {
            "setup" => {
                let mut broker = open_detached_fixture_broker(&root);
                broker.ensure_artifact_sweep_namespace().unwrap();
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
                // Flat leaves make the deliberate zero-budget crash deterministic:
                // one leaf is removed before interruption; directory descent
                // remains forbidden after the deadline has already expired.
                std::fs::create_dir_all(&closed_cache).unwrap();
                std::fs::write(
                    closed_cache.join("CACHEDIR.TAG"),
                    "Signature: 8a477f597d28d172\n",
                )
                .unwrap();
                for index in 0..128 {
                    std::fs::write(
                        closed_cache.join(format!("entry-{index:03}")),
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
                let idle = broker
                    .start_worktree("idle open cache fixture", None)
                    .unwrap();
                let idle_worktree = PathBuf::from(&idle.worktree_path);
                let idle_cache = idle_worktree.join("target");
                let nested = idle_cache.join("debug/incremental/hash");
                std::fs::create_dir_all(&nested).unwrap();
                std::fs::write(
                    idle_cache.join("CACHEDIR.TAG"),
                    "Signature: 8a477f597d28d172\n",
                )
                .unwrap();
                let old_time = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
                let old_cache_paths = [
                    idle_cache.clone(),
                    idle_cache.join("CACHEDIR.TAG"),
                    idle_cache.join("debug"),
                ];
                for path in &old_cache_paths {
                    std::fs::File::open(path)
                        .unwrap()
                        .set_modified(old_time)
                        .unwrap();
                }
                std::fs::write(nested.join("fresh.bin"), b"recent deep write").unwrap();
                let quiet_since = now_ms() - 48 * 60 * 60 * 1000;
                broker
                    .store()
                    .connection_mut()
                    .execute(
                        "UPDATE sessions SET created_at = ?2, last_activity_at = ?2 WHERE id = ?1",
                        rusqlite::params![idle.id, quiet_since],
                    )
                    .unwrap();
                if let Some(name) = idle_worktree.file_name() {
                    for file in ["index", "HEAD"] {
                        let path = root.join(".git/worktrees").join(name).join(file);
                        if path.exists() {
                            std::fs::File::open(path)
                                .unwrap()
                                .set_modified(old_time)
                                .unwrap();
                        }
                    }
                }
                for (name, value) in [
                    ("closed-worktree", closed.worktree_path.as_str()),
                    ("active-worktree", active.worktree_path.as_str()),
                    ("idle-worktree", idle.worktree_path.as_str()),
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
                let mut command = detached_fixture_command(&root, "crash-child");
                configure_artifact_worker_process_group(&mut command);
                let mut child = command.spawn().unwrap();
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
                // Keep this process, and therefore its process group, live
                // until the parent test cancels that exact owned group. If
                // the worker didn't get its own group, teardown kills it too.
                #[cfg(unix)]
                assert!(wait_for_fixture_marker(
                    &detached_fixture_marker(&root, "launcher-release"),
                    Duration::from_secs(20),
                ));
            }
            "crash-child" => {
                let lock = acquire_artifact_sweep_worker_lock(&root).unwrap();
                let mut broker = open_detached_fixture_broker(&root);
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
                let idle_worktree = PathBuf::from(
                    std::fs::read_to_string(detached_fixture_marker(&root, "idle-worktree"))
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
                let idle_cache = idle_worktree.join("target");
                assert!(cache.is_dir());
                assert!(active_cache.join("debug/keep.txt").is_file());
                assert!(
                    idle_cache
                        .join("debug/incremental/hash/fresh.bin")
                        .is_file()
                );
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
                let mut entries_reclaimed: usize = 0;
                {
                    let _interrupt_after_first_unlink =
                        ArtifactSweepTestMutationInterrupt::after_first_unlink(&cache);
                    broker
                        .run_artifact_sweep_recovery_worker_loop(
                            &policy,
                            77,
                            Some(1),
                            None,
                            |broker, policy, _remaining| {
                                let result = broker
                                    .sweep_artifacts_autonomously_with_test_deadlines(
                                        policy,
                                        ArtifactSweepScope::AutomaticContinuation,
                                        Instant::now() + Duration::from_secs(5),
                                        Instant::now() + Duration::from_secs(30),
                                    );
                                if let Ok((_, count)) = &result {
                                    entries_reclaimed = *count;
                                }
                                result
                            },
                        )
                        .unwrap();
                }
                assert!(
                    entries_reclaimed > 0,
                    "the detached cache-only child must make filesystem progress"
                );
                assert_eq!(
                    broker
                        .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                        .unwrap()
                        .as_deref(),
                    Some("0"),
                    "an incomplete worker pass must not advance the completed generation"
                );
                assert!(
                    cache.is_dir(),
                    "partial cache deletion must remain resumable"
                );
                let queued_after_partial = decode_interrupted_paths(
                    broker
                        .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                        .unwrap()
                        .as_deref(),
                )
                .unwrap();
                assert!(queued_after_partial.contains(&cache.to_string_lossy().into_owned()));
                assert!(active_cache.is_dir(), "a live session cache is protected");
                assert!(
                    idle_cache
                        .join("debug/incremental/hash/fresh.bin")
                        .is_file(),
                    "an idle open cache with a deep write stays protected"
                );

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
                let mut broker = open_detached_fixture_broker(&root);
                let closed_worktree = PathBuf::from(
                    std::fs::read_to_string(detached_fixture_marker(&root, "closed-worktree"))
                        .unwrap(),
                );
                let active_worktree = PathBuf::from(
                    std::fs::read_to_string(detached_fixture_marker(&root, "active-worktree"))
                        .unwrap(),
                );
                let idle_worktree = PathBuf::from(
                    std::fs::read_to_string(detached_fixture_marker(&root, "idle-worktree"))
                        .unwrap(),
                );
                let closed_session_id =
                    std::fs::read_to_string(detached_fixture_marker(&root, "closed-session-id"))
                        .unwrap();
                let cache = closed_worktree.join("target");
                let active_cache = active_worktree.join("target");
                let idle_cache = idle_worktree.join("target");
                {
                    let requested = broker
                        .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                        .unwrap()
                        .unwrap();
                    assert_eq!(requested, "77");
                    assert_eq!(
                        broker
                            .artifact_sweep_meta_get(ARTIFACT_SWEEP_CURSOR_KEY)
                            .unwrap()
                            .as_deref(),
                        Some(closed_session_id.trim()),
                        "the restart must see the saved session cursor"
                    );
                    assert_eq!(
                        broker
                            .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                            .unwrap()
                            .as_deref(),
                        Some(cache.to_string_lossy().as_ref()),
                        "the restart must see the interrupted cache path"
                    );
                }

                let policy = load_retention_policy(broker.main_root()).unwrap();
                let mut entries_reclaimed: usize = 0;
                broker
                    .run_artifact_sweep_recovery_worker_loop(
                        &policy,
                        77,
                        None,
                        None,
                        |broker, policy, remaining| {
                            let deadline = remaining.map(|remaining| Instant::now() + remaining);
                            let result = broker.sweep_artifacts_autonomously_with_deadline(
                                policy,
                                ArtifactSweepScope::AutomaticContinuation,
                                deadline,
                            );
                            if let Ok((_, count)) = &result {
                                entries_reclaimed = entries_reclaimed.saturating_add(*count);
                            }
                            result
                        },
                    )
                    .unwrap();
                assert!(entries_reclaimed > 0);
                assert!(!cache.exists(), "the closed session cache is reclaimed");
                assert!(active_cache.join("debug/keep.txt").is_file());
                assert!(
                    idle_cache
                        .join("debug/incremental/hash/fresh.bin")
                        .is_file(),
                    "the idle open cache remains protected after recovery"
                );
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
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    broker
                        .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
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

    #[cfg(unix)]
    #[test]
    fn artifact_worker_lock_refuses_hard_links_without_mutating_the_target() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let run = root.join(".aethyme/run");
        std::fs::create_dir_all(&run).unwrap();
        let protected = root.join("protected-data");
        std::fs::write(&protected, b"keep this file intact\n").unwrap();
        let lock_path = run.join("gc-artifact-worker.lock");
        std::fs::hard_link(&protected, &lock_path).unwrap();

        assert!(artifact_sweep_worker_lock_file(root).is_err());
        assert_eq!(
            std::fs::read(&protected).unwrap(),
            b"keep this file intact\n",
            "the worker must never truncate a hard-linked target"
        );
    }

    #[test]
    fn detached_worker_survives_its_launcher_and_resumes_after_a_crash() {
        let tmp = tempfile::tempdir().unwrap();
        let root_path = tmp.path().canonicalize().unwrap();
        let root = root_path.as_path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
        git(root, &["add", "README.md", ".gitignore"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 30\nclosed_worktree_grace_hours = 0\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\nartifact_sweep_interval_hours = 24\nidle_session_artifact_hours = 24\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".aethyme/test-worker")).unwrap();

        let worktree_root = tempfile::tempdir().unwrap();
        let host_state = tempfile::tempdir().unwrap();
        let host_cache = tempfile::tempdir().unwrap();
        let preparation_cache = tempfile::tempdir().unwrap();
        for (name, path) in [
            ("worktree-root", worktree_root.path()),
            ("host-state", host_state.path()),
            ("host-cache", host_cache.path()),
            ("preparation-cache", preparation_cache.path()),
        ] {
            let path = path.canonicalize().unwrap();
            let path = if name == "worktree-root" {
                let selected = path.join("worktrees");
                assert!(!selected.exists());
                selected
            } else {
                path
            };
            std::fs::write(
                detached_fixture_marker(root, name),
                path.to_string_lossy().as_ref(),
            )
            .unwrap();
        }
        // Seed a valid operator-authorized journal before the fixture child
        // opens the broker in worker mode. Broker::open must return through
        // the worker-only early exit without resuming or rewriting it.
        let journal_path = root.join(".aethyme/gc-journal.json");
        let journal_before = {
            let mut broker = open_detached_fixture_broker(root);
            let journal = GcJournal::from(broker.gc_plan().unwrap());
            write_journal(&journal_path, &journal).unwrap();
            std::fs::read(&journal_path).unwrap()
        };
        let setup = detached_fixture_command(root, "setup").status().unwrap();
        assert!(setup.success());
        assert_eq!(
            std::fs::read(&journal_path).unwrap(),
            journal_before,
            "Broker::open in detached worker mode must leave operator GC journals untouched"
        );
        let closed_worktree = PathBuf::from(
            std::fs::read_to_string(detached_fixture_marker(root, "closed-worktree")).unwrap(),
        );
        let active_worktree = PathBuf::from(
            std::fs::read_to_string(detached_fixture_marker(root, "active-worktree")).unwrap(),
        );
        let closed_cache = closed_worktree.join("target");
        let active_cache = active_worktree.join("target");

        {
            let namespace = detached_fixture_namespace(root);
            let store = detached_fixture_store(root);
            store
                .meta_set(
                    &scoped_artifact_sweep_key(&namespace, ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY),
                    "77",
                )
                .unwrap();
            store
                .meta_set(
                    &scoped_artifact_sweep_key(&namespace, ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY),
                    "0",
                )
                .unwrap();
            store
                .meta_set(
                    &scoped_artifact_sweep_key(&namespace, ARTIFACT_SWEEP_CURSOR_KEY),
                    "0",
                )
                .unwrap();
            store
                .meta_set(
                    &scoped_artifact_sweep_key(&namespace, ARTIFACT_SWEEP_PROGRESS_KEY),
                    "1",
                )
                .unwrap();
            store
                .meta_set(
                    &scoped_artifact_sweep_key(&namespace, ARTIFACT_SWEEP_INTERRUPTED_KEY),
                    "",
                )
                .unwrap();
        }
        let cache_entries_before = std::fs::read_dir(&closed_cache).unwrap().count();

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
            std::fs::read_dir(&closed_cache).unwrap().count(),
            cache_entries_before
        );
        drop(held_lock);

        // Run the worker launcher as a supervised user command. The real
        // cleanup watcher tears down that owned group after the command and
        // supervisor exit; the worker must use the production process-group
        // configuration to survive it.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            let executable = std::env::current_exe().unwrap();
            let command_text = "exec \"$AETHYME_TEST_DETACHED_GC_EXECUTABLE\" --exact gc::tests::detached_artifact_worker_child_fixture --nocapture --quiet";
            let log_path = root.join(".aethyme/test-worker/agent-supervisor.log");
            let log = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(log_path)
                .unwrap();
            let (mut supervisor_command, nonce) =
                crate::agent_supervisor::process(command_text, root, &log, None).unwrap();
            supervisor_command
                .current_dir(root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(log)
                .env(DETACHED_FIXTURE_MODE_ENV, "launch-crash-child")
                .env(DETACHED_FIXTURE_ROOT_ENV, root)
                .env("AETHYME_TEST_DETACHED_GC_EXECUTABLE", executable)
                .process_group(0);
            let mut supervisor = supervisor_command.spawn().unwrap();
            let supervisor_group = i32::try_from(supervisor.id()).unwrap();
            crate::agent_supervisor::await_ready(supervisor.stdout.as_mut().unwrap(), &nonce)
                .unwrap();
            supervisor.stdout.take();
            writeln!(
                supervisor.stdin.take().unwrap(),
                "{}",
                crate::agent_supervisor::GO
            )
            .unwrap();
            assert!(wait_for_fixture_marker(
                &detached_fixture_marker(root, "child-ready"),
                Duration::from_secs(10),
            ));
            std::fs::write(
                detached_fixture_marker(root, "launcher-release"),
                b"release",
            )
            .unwrap();
            assert!(supervisor.wait().unwrap().success());
            let group_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                // SAFETY: signal zero only probes the captured positive child
                // process-group ID and never delivers a signal after its leader
                // is reaped. ESRCH proves teardown; unexpected errors refuse.
                let result = unsafe { libc::kill(-supervisor_group, 0) };
                if result == -1 {
                    let error = std::io::Error::last_os_error();
                    match error.raw_os_error() {
                        Some(libc::ESRCH) => break,
                        Some(libc::EINTR) => {}
                        _ => panic!("could not prove supervisor group teardown: {error}"),
                    }
                }
                assert!(
                    Instant::now() < group_deadline,
                    "supervisor process group remained after its cleanup watcher completed"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        #[cfg(not(unix))]
        {
            let launcher = detached_fixture_command(root, "launch-crash-child")
                .status()
                .unwrap();
            assert!(launcher.success());
        }
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
        let partially_remaining = std::fs::read_dir(&closed_cache).unwrap().count();
        assert!(partially_remaining < cache_entries_before && partially_remaining > 0);
        assert!(active_cache.join("debug/keep.txt").is_file());
        {
            let namespace = detached_fixture_namespace(root);
            let store = detached_fixture_store(root);
            assert_eq!(
                store
                    .meta_get(&scoped_artifact_sweep_key(
                        &namespace,
                        ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY,
                    ))
                    .unwrap()
                    .as_deref(),
                Some("77")
            );
            assert_eq!(
                store
                    .meta_get(&scoped_artifact_sweep_key(
                        &namespace,
                        ARTIFACT_SWEEP_CURSOR_KEY
                    ))
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
                    .meta_get(&scoped_artifact_sweep_key(
                        &namespace,
                        ARTIFACT_SWEEP_INTERRUPTED_KEY,
                    ))
                    .unwrap()
                    .as_deref(),
                Some(closed_cache.to_string_lossy().as_ref())
            );
        }

        let resumed = detached_fixture_command(root, "resume-child")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert!(
            resumed.status.success(),
            "resume child failed with status {:?}: stdout={} stderr={}",
            resumed.status.code(),
            String::from_utf8_lossy(&resumed.stdout),
            String::from_utf8_lossy(&resumed.stderr),
        );
        assert!(detached_fixture_marker(root, "child-resumed").is_file());
        assert!(!closed_cache.exists());
        assert!(active_cache.join("debug/keep.txt").is_file());
        assert!(closed_worktree.is_dir());
        assert!(GitRepo::discover(&closed_worktree).is_ok());
        assert!(root.join("README.md").is_file());
        assert!(root.join(".git/HEAD").is_file());
        let namespace = detached_fixture_namespace(root);
        let store = detached_fixture_store(root);
        assert_eq!(
            store
                .meta_get(&scoped_artifact_sweep_key(
                    &namespace,
                    ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY,
                ))
                .unwrap()
                .as_deref(),
            Some("77"),
            "the request is durable history; completion advances separately"
        );
        assert_eq!(
            store
                .meta_get(&scoped_artifact_sweep_key(
                    &namespace,
                    ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY,
                ))
                .unwrap()
                .as_deref(),
            Some("77")
        );
        assert_eq!(
            store
                .meta_get(&scoped_artifact_sweep_key(
                    &namespace,
                    ARTIFACT_SWEEP_CURSOR_KEY
                ))
                .unwrap()
                .as_deref(),
            Some("0")
        );
        assert_eq!(
            store
                .meta_get(&scoped_artifact_sweep_key(
                    &namespace,
                    ARTIFACT_SWEEP_INTERRUPTED_KEY,
                ))
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

        assert_eq!(
            interrupted_tree_state(root, &root.join(".pnpm-store"), &[]),
            InterruptedTreeState::Resume
        );
        assert_eq!(
            interrupted_tree_state(root, &root.join("web/node_modules"), &[]),
            InterruptedTreeState::Resume
        );
        // Component guard only: canonically inside root, no symlink, an
        // artefact name -- but it is spelled with `..`.
        assert_eq!(
            interrupted_tree_state(root, &root.join("a/../.pnpm-store"), &[]),
            InterruptedTreeState::Revoked("unsafe_path_components")
        );
        // Name guard only: a real directory inside root, but not a cache.
        assert_eq!(
            interrupted_tree_state(root, &root.join("src"), &[]),
            InterruptedTreeState::Revoked("not_unattended_artifact")
        );
        assert_eq!(
            interrupted_tree_state(root, &root.join("src"), &["src".to_string()]),
            InterruptedTreeState::Revoked("not_unattended_artifact")
        );
        std::fs::create_dir(root.join("custom-cache")).unwrap();
        assert_eq!(
            interrupted_tree_state(
                root,
                &root.join("custom-cache"),
                &["custom-cache".to_string()]
            ),
            InterruptedTreeState::Resume
        );
        // Symlink guard only: the link resolves inside root.
        assert_eq!(
            interrupted_tree_state(root, &root.join("inner-link/node_modules"), &[]),
            InterruptedTreeState::Blocked("symlink_component")
        );
        // A link leaving root.
        assert_eq!(
            interrupted_tree_state(root, &root.join("link/node_modules"), &[]),
            InterruptedTreeState::Blocked("symlink_component")
        );
        // Not under root at all, and root itself.
        assert_eq!(
            interrupted_tree_state(root, &outside.path().join("node_modules"), &[]),
            InterruptedTreeState::Revoked("outside_worktree")
        );
        assert_eq!(
            interrupted_tree_state(root, root, &[]),
            InterruptedTreeState::Revoked("unsafe_path_components")
        );
        // Gone.
        assert_eq!(
            interrupted_tree_state(root, &root.join("node_modules"), &[]),
            InterruptedTreeState::Revoked("path_missing")
        );
    }

    /// Explicit reclaim commands keep the pressure-scaled policy budget.
    #[test]
    fn explicit_and_closed_session_sweeps_keep_the_pressure_scaled_budget() {
        for scope in [
            ArtifactSweepScope::Drain,
            ArtifactSweepScope::FinishDrain,
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
            "schema_version": GC_LEGACY_PLAN_SCHEMA_VERSION,
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
        assert_eq!(parsed.age_anchor_ms, None);
        assert_eq!(
            gc_journal_age_reference_ms(&parsed).unwrap(),
            parsed.evaluated_at
        );
        assert!(gc_journal_schema_is_resumable(parsed.schema_version));
    }

    #[test]
    fn schema_four_gc_journals_require_the_bound_utc_day_anchor() {
        let evaluated_at = 42 * MILLIS_PER_DAY + 12_345;
        let mut plan: GcPlan = serde_json::from_value(serde_json::json!({
            "schema_version": GC_PLAN_SCHEMA_VERSION,
            "digest": "a".repeat(64),
            "evaluated_at": evaluated_at,
            "age_anchor_ms": utc_day_floor_ms(evaluated_at),
            "policy": RetentionPolicy::default(),
            "rows": [],
            "files": [],
            "worktrees": [],
            "artifacts": [],
            "orphans": [],
            "blockers": [],
            "estimated_reclaimable_bytes": 0,
            "estimated_retained_bytes": 0,
            "estimated_blocked_bytes": 0
        }))
        .unwrap();
        let journal = GcJournal::from(plan.clone());
        assert_eq!(
            gc_journal_age_reference_ms(&journal).unwrap(),
            utc_day_floor_ms(evaluated_at)
        );

        plan.age_anchor_ms = None;
        let missing = GcJournal::from(plan.clone());
        assert!(gc_journal_age_reference_ms(&missing).is_err());

        plan.age_anchor_ms = Some(utc_day_floor_ms(evaluated_at) - MILLIS_PER_DAY);
        let invalid = GcJournal::from(plan);
        assert!(gc_journal_age_reference_ms(&invalid).is_err());
    }

    #[test]
    fn gc_journal_archive_budget_has_literal_count_and_byte_boundaries() {
        assert!(gc_journal_archive_budget_admits(31, 0, 67_108_864));
        assert!(!gc_journal_archive_budget_admits(32, 0, 0));
        assert!(!gc_journal_archive_budget_admits(31, 67_108_864, 1));
        assert!(!gc_journal_archive_budget_admits(0, 0, 67_108_865));
    }

    #[test]
    fn gc_journal_archive_scans_have_literal_64_65_entry_boundary() {
        // Archive inventory, temporary cleanup, and hardlink recovery share
        // this predicate so each directory scan stops after the same bound.
        assert!(gc_journal_archive_directory_scan_has_budget(64));
        assert!(!gc_journal_archive_directory_scan_has_budget(65));
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn gc_abandon_wrong_digest_preserves_the_pending_journal_without_archive() {
        let (_fixture, mut broker, root, digest) = registered_orphan_fixture();
        let journal_path = broker.main_root().join(".aethyme/gc-journal.json");
        let original = std::fs::read(&journal_path).unwrap();
        let wrong_digest = if digest.starts_with('a') {
            "b".repeat(64)
        } else {
            "a".repeat(64)
        };

        let error = broker.gc_abandon_pending(&wrong_digest).unwrap_err();
        assert!(matches!(
            error,
            BrokerOpError::GcAbandonConfirmationMismatch { .. }
        ));
        assert_eq!(std::fs::read(&journal_path).unwrap(), original);
        assert!(
            !broker
                .main_root()
                .join(".aethyme/gc-journal-archives")
                .exists()
        );
        assert!(root.join("retained.txt").is_file());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn gc_abandon_refuses_a_mismatched_archive_collision_without_clearing_journal() {
        use std::os::unix::fs::PermissionsExt;

        let (_fixture, mut broker, root, digest) = registered_orphan_fixture();
        let journal_path = broker.main_root().join(".aethyme/gc-journal.json");
        let original = std::fs::read(&journal_path).unwrap();
        let state_directory = broker.main_root().join(".aethyme");
        let state_identity = gc_state_directory_identity(&state_directory).unwrap();
        let (archive_directory, _) =
            ensure_gc_journal_archive_directory(&state_directory, state_identity).unwrap();
        let archive_path = archive_directory.join(format!(
            "{GC_JOURNAL_ARCHIVE_PREFIX}{digest}{GC_JOURNAL_ARCHIVE_SUFFIX}"
        ));
        std::fs::write(&archive_path, b"different journal bytes").unwrap();
        std::fs::set_permissions(&archive_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let error = broker.gc_abandon_pending(&digest).unwrap_err();
        assert!(matches!(
            error,
            BrokerOpError::GcJournalAbandonRefused { .. }
        ));
        assert_eq!(std::fs::read(&journal_path).unwrap(), original);
        assert_eq!(
            std::fs::read(&archive_path).unwrap(),
            b"different journal bytes"
        );
        assert!(root.join("retained.txt").is_file());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn gc_abandon_revokes_schema_two_without_removing_retained_data() {
        let (_fixture, mut broker, root, _initial_digest) = registered_orphan_fixture();
        let journal_path = broker.main_root().join(".aethyme/gc-journal.json");
        let mut old_plan = broker.gc_plan().unwrap();
        old_plan.schema_version = 2;
        old_plan.age_anchor_ms = None;
        old_plan.finish_digest().unwrap();
        let old_digest = old_plan.digest.clone();
        let mut old_journal = GcJournal::from(old_plan);
        old_journal.schema_version = 2;
        write_journal(&journal_path, &old_journal).unwrap();
        let exact_legacy_bytes = std::fs::read(&journal_path).unwrap();

        let resume_error = broker.gc_apply(&old_digest).unwrap_err();
        assert!(matches!(
            &resume_error,
            BrokerOpError::InvalidGcJournal { reason }
                if reason.contains("schema 2 is no longer resumable")
                    && reason.contains("gc abandon --confirm")
        ));
        assert_eq!(std::fs::read(&journal_path).unwrap(), exact_legacy_bytes);
        assert!(root.join("retained.txt").is_file());

        let report = broker.gc_abandon_pending(&old_digest).unwrap();
        assert_eq!(report.digest, old_digest);
        assert!(!journal_path.exists());
        assert_eq!(
            std::fs::read(broker.main_root().join(&report.archive_path)).unwrap(),
            exact_legacy_bytes
        );
        assert!(root.join("retained.txt").is_file());
        let fresh_plan = broker.gc_plan().unwrap();
        assert_eq!(fresh_plan.schema_version, 4);
        assert_ne!(fresh_plan.digest, old_digest);
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn gc_apply_resumes_legacy_schema_three_journal_without_age_anchor() {
        let (_fixture, mut broker, root, _initial_digest) = registered_orphan_fixture();
        let journal_path = broker.main_root().join(".aethyme/gc-journal.json");
        std::fs::remove_file(&journal_path).unwrap();

        let mut legacy_plan = broker.gc_plan().unwrap();
        legacy_plan.schema_version = GC_LEGACY_PLAN_SCHEMA_VERSION;
        legacy_plan.age_anchor_ms = None;
        legacy_plan.finish_digest().unwrap();
        let digest = legacy_plan.digest.clone();
        write_journal(&journal_path, &GcJournal::from(legacy_plan)).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        assert_eq!(saved["schema_version"], GC_LEGACY_PLAN_SCHEMA_VERSION);
        assert!(saved.get("age_anchor_ms").is_none());

        let report = broker.gc_apply(&digest).unwrap();
        assert!(report.complete, "{report:?}");
        assert!(!root.exists());
        assert!(!journal_path.exists());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn gc_forget_abandoned_allows_same_digest_new_cursor_then_abandon() {
        let (_fixture, mut broker, root, digest) = registered_orphan_fixture();
        let journal_path = broker.main_root().join(".aethyme/gc-journal.json");
        let first_bytes = std::fs::read(&journal_path).unwrap();
        let first = broker.gc_abandon_pending(&digest).unwrap();
        let archive_path = broker.main_root().join(&first.archive_path);
        assert_eq!(std::fs::read(&archive_path).unwrap(), first_bytes);

        // A plan digest identifies the reviewed plan, not the journal cursor.
        // Model a later journal with that same plan digest and newer progress.
        let mut next_journal: GcJournal = serde_json::from_slice(&first_bytes).unwrap();
        next_journal.rows_removed = next_journal.rows_removed.saturating_add(1);
        write_journal(&journal_path, &next_journal).unwrap();
        let next_bytes = std::fs::read(&journal_path).unwrap();
        assert_ne!(next_bytes, first_bytes);

        let collision = broker.gc_abandon_pending(&digest).unwrap_err();
        assert!(matches!(
            collision,
            BrokerOpError::GcJournalAbandonRefused { .. }
        ));
        assert_eq!(std::fs::read(&journal_path).unwrap(), next_bytes);
        assert_eq!(std::fs::read(&archive_path).unwrap(), first_bytes);

        broker.gc_forget_abandoned(&digest).unwrap();
        assert!(!archive_path.exists());
        assert_eq!(std::fs::read(&journal_path).unwrap(), next_bytes);

        let second = broker.gc_abandon_pending(&digest).unwrap();
        assert!(!journal_path.exists());
        assert_eq!(
            std::fs::read(broker.main_root().join(second.archive_path)).unwrap(),
            next_bytes
        );
        assert!(root.join("retained.txt").is_file());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn gc_abandon_reuses_exact_crash_window_archive_and_forget_only_removes_journal() {
        use std::os::unix::fs::PermissionsExt;

        let (_fixture, mut broker, root, digest) = registered_orphan_fixture();
        let journal_path = broker.main_root().join(".aethyme/gc-journal.json");
        let exact_pending_bytes = std::fs::read(&journal_path).unwrap();
        let state_directory = broker.main_root().join(".aethyme");
        let state_identity = gc_state_directory_identity(&state_directory).unwrap();
        let (archive_directory, archive_identity) =
            ensure_gc_journal_archive_directory(&state_directory, state_identity).unwrap();
        let archive_path = archive_directory.join(format!(
            "{GC_JOURNAL_ARCHIVE_PREFIX}{digest}{GC_JOURNAL_ARCHIVE_SUFFIX}"
        ));
        archive_gc_journal_bytes(
            &archive_directory,
            archive_identity,
            &archive_path,
            &exact_pending_bytes,
        )
        .unwrap();

        let forget_error = broker.gc_forget_abandoned(&digest).unwrap_err();
        assert!(matches!(
            forget_error,
            BrokerOpError::GcJournalAbandonRefused { .. }
        ));
        assert!(
            journal_path.exists(),
            "the active crash-window journal remains"
        );
        assert_eq!(std::fs::read(&archive_path).unwrap(), exact_pending_bytes);

        // Model termination after the synced temporary file was hard-linked
        // into its deterministic archive name but before the temporary name
        // was dropped. The public retry must verify and retire only this exact
        // same-inode temporary link before it clears the active journal.
        std::fs::remove_file(&archive_path).unwrap();
        sync_gc_state_directory(&archive_directory, archive_identity).unwrap();
        let archive_name = archive_path.file_name().unwrap().to_str().unwrap();
        let temporary_path = archive_directory.join(format!(".{archive_name}-crash-window"));
        let mut temporary = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .unwrap();
        use std::io::Write;
        temporary.write_all(&exact_pending_bytes).unwrap();
        temporary.sync_all().unwrap();
        drop(temporary);
        std::fs::set_permissions(&temporary_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&temporary_path, &archive_path).unwrap();
        sync_gc_state_directory(&archive_directory, archive_identity).unwrap();

        let report = broker.gc_abandon_pending(&digest).unwrap();
        assert_eq!(report.digest, digest);
        assert!(!journal_path.exists());
        assert_eq!(std::fs::read(&archive_path).unwrap(), exact_pending_bytes);
        assert!(!temporary_path.exists());
        assert!(root.join("retained.txt").is_file());

        let forgotten = broker.gc_forget_abandoned(&digest).unwrap();
        assert_eq!(forgotten.digest, digest);
        assert_eq!(forgotten.bytes_forgotten, exact_pending_bytes.len() as u64);
        assert!(!archive_path.exists());
        assert!(root.join("retained.txt").is_file());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn gc_forget_abandoned_retires_private_partial_and_synced_temporary_files() {
        use std::os::unix::fs::PermissionsExt;

        let (_fixture, mut broker, root, digest) = registered_orphan_fixture();
        let journal_path = broker.main_root().join(".aethyme/gc-journal.json");
        let exact_active_bytes = std::fs::read(&journal_path).unwrap();
        let state_directory = broker.main_root().join(".aethyme");
        let state_identity = gc_state_directory_identity(&state_directory).unwrap();
        let (archive_directory, archive_identity) =
            ensure_gc_journal_archive_directory(&state_directory, state_identity).unwrap();
        let archive_name =
            format!("{GC_JOURNAL_ARCHIVE_PREFIX}{digest}{GC_JOURNAL_ARCHIVE_SUFFIX}");
        let temporary_paths = [
            archive_directory.join(format!(".{archive_name}-partial1")),
            archive_directory.join(format!(".{archive_name}-synced1")),
        ];
        let partial_bytes = b"{\"schema_version\":";
        for (path, bytes) in temporary_paths
            .iter()
            .zip([partial_bytes.as_slice(), exact_active_bytes.as_slice()])
        {
            std::fs::write(path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        sync_gc_state_directory(&archive_directory, archive_identity).unwrap();

        let blocked = broker.gc_abandon_pending(&digest).unwrap_err();
        assert!(matches!(
            blocked,
            BrokerOpError::GcJournalAbandonRefused { .. }
        ));
        assert_eq!(std::fs::read(&journal_path).unwrap(), exact_active_bytes);

        let report = broker.gc_forget_abandoned(&digest).unwrap();
        assert_eq!(report.digest, digest);
        assert_eq!(
            report.bytes_forgotten,
            (partial_bytes.len() + exact_active_bytes.len()) as u64
        );
        assert!(temporary_paths.iter().all(|path| !path.exists()));
        assert_eq!(std::fs::read(&journal_path).unwrap(), exact_active_bytes);
        let abandoned = broker.gc_abandon_pending(&digest).unwrap();
        assert!(!journal_path.exists());
        assert_eq!(
            std::fs::read(broker.main_root().join(abandoned.archive_path)).unwrap(),
            exact_active_bytes
        );
        assert!(root.join("retained.txt").is_file());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn gc_forget_refuses_suspicious_staging_without_unlinking_archive() {
        use std::os::unix::fs::PermissionsExt;

        let (_fixture, mut broker, root, digest) = registered_orphan_fixture();
        let report = broker.gc_abandon_pending(&digest).unwrap();
        let archive_path = broker.main_root().join(&report.archive_path);
        let archive_bytes = std::fs::read(&archive_path).unwrap();
        let archive_name = archive_path.file_name().unwrap().to_str().unwrap();
        let temporary_path = archive_path
            .parent()
            .unwrap()
            .join(format!(".{archive_name}-not_a_temp"));
        std::fs::write(&temporary_path, b"suspicious staging bytes").unwrap();
        std::fs::set_permissions(&temporary_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let error = broker.gc_forget_abandoned(&digest).unwrap_err();
        assert!(matches!(
            error,
            BrokerOpError::GcJournalAbandonRefused { .. }
        ));
        assert_eq!(std::fs::read(&archive_path).unwrap(), archive_bytes);
        assert_eq!(
            std::fs::read(&temporary_path).unwrap(),
            b"suspicious staging bytes"
        );
        assert!(root.join("retained.txt").is_file());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn gc_abandon_refuses_unknown_future_journal_schema() {
        let (_fixture, mut broker, root, digest) = registered_orphan_fixture();
        let journal_path = broker.main_root().join(".aethyme/gc-journal.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        value["schema_version"] = serde_json::json!(5);
        let mut unknown_bytes = serde_json::to_vec_pretty(&value).unwrap();
        unknown_bytes.push(b'\n');
        std::fs::write(&journal_path, &unknown_bytes).unwrap();

        let error = broker.gc_abandon_pending(&digest).unwrap_err();
        assert!(matches!(error, BrokerOpError::InvalidGcJournal { .. }));
        assert_eq!(std::fs::read(&journal_path).unwrap(), unknown_bytes);
        assert!(
            !broker
                .main_root()
                .join(".aethyme/gc-journal-archives")
                .exists()
        );
        assert!(root.join("retained.txt").is_file());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn registered_orphan_fixture() -> (tempfile::TempDir, Broker, PathBuf, String) {
        let fixture = tempfile::tempdir().unwrap();
        let repo = fixture.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        std::fs::create_dir(repo.join(".aethyme")).unwrap();
        std::fs::write(
            repo.join(".aethyme/broker.toml"),
            "[retention]\norphan_worktree_roots_days = 0\nartifact_sweep_budget_ms = 0\n",
        )
        .unwrap();
        let database = fixture.path().join("host-state/host-operations.db");
        // This constructor performs local migration/backfill, without startup
        // maintenance, so host coordination is private before any GC work.
        let mut broker = Broker::open_for_compatibility_backfill(&repo)
            .unwrap()
            .with_host_operation_database(&database)
            .with_worktree_root(fixture.path().join("current-root"));
        let origin = fixture.path().join("disappearing-origin");
        let root = fixture.path().join("historical-placement/exact-root");
        std::fs::create_dir(&origin).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let marker = crate::broker::WorktreeRootMarker {
            schema_version: crate::broker::WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: "historic-owned-root".into(),
            repository_root: origin.clone(),
        };
        std::fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("retained.txt"),
            "retained until reviewed deletion\n",
        )
        .unwrap();
        crate::worktree_registry::register(
            &database,
            &root,
            &marker,
            crate::WorktreeRootSource::LibraryOverride,
        )
        .unwrap();
        std::fs::remove_dir(&origin).unwrap();
        let plan = broker.gc_plan().unwrap();
        assert_eq!(
            plan.orphans.len(),
            1,
            "historical placement must be discovered"
        );
        let digest = plan.digest.clone();
        write_journal(
            &repo.join(".aethyme/gc-journal.json"),
            &GcJournal::from(plan),
        )
        .unwrap();
        (fixture, broker, root.canonicalize().unwrap(), digest)
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn a_discovery_parent_alias_matches_one_registered_root_without_unknown_bytes() {
        let (fixture, broker, root, _digest) = registered_orphan_fixture();
        let alias = fixture.path().join("aliased-container");
        std::os::unix::fs::symlink(root.parent().unwrap(), &alias).unwrap();
        let mut broker = broker.with_worktree_root(alias.join(root.file_name().unwrap()));
        let plan = broker.gc_plan().unwrap();
        assert_eq!(plan.orphans.len(), 1, "aliased roots must be counted once");
        assert_eq!(plan.orphans[0].worktree_root, root.to_string_lossy());
        assert!(
            !plan
                .deferred_checks
                .iter()
                .any(|check| check == "orphan_scan"),
            "a registered root reached through a parent alias has known ownership: {:?}",
            plan.blockers
        );
        assert!(
            !plan
                .blockers
                .iter()
                .any(|blocker| blocker.kind == "worktree_origin_uninspected")
        );
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn a_busy_orphan_claim_stays_journaled_until_a_later_apply_reclaims_it() {
        let (_fixture, mut broker, root, digest) = registered_orphan_fixture();
        let journal_path = broker.main_root().join(".aethyme/gc-journal.json");
        let journal = load_journal(&journal_path).unwrap().unwrap();
        let database = broker.host_operation_database_path().unwrap();
        let held = crate::worktree_registry::claim_for_removal(
            &database,
            &root,
            journal.remaining_orphans[0]
                .ownership_fingerprint
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        let blocked = broker.gc_apply(&digest).unwrap();
        assert!(
            !blocked.complete,
            "contention must not be reported as completed deletion"
        );
        assert!(blocked.recovery_action.is_some());
        assert!(root.join("retained.txt").is_file());
        assert_eq!(
            load_journal(&journal_path)
                .unwrap()
                .unwrap()
                .remaining_orphans
                .len(),
            1
        );
        drop(held);
        let completed = broker.gc_apply(&digest).unwrap();
        assert!(completed.complete, "{completed:?}");
        assert_eq!(
            completed.orphans_removed,
            vec![root.to_string_lossy().into_owned()]
        );
        assert!(!root.exists());
        assert!(!journal_path.exists());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn orphan_deletion_io_failure_stays_journaled_and_resumes_after_recovery() {
        use std::os::unix::fs::PermissionsExt;

        struct RestorePermissions(PathBuf, std::fs::Permissions);
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                std::fs::set_permissions(&self.0, self.1.clone()).unwrap();
            }
        }

        // This regression must exercise a real permission failure, not pass
        // vacuously under an identity that bypasses filesystem permissions.
        assert_ne!(
            // SAFETY: geteuid has no arguments or pointer preconditions.
            unsafe { libc::geteuid() },
            0,
            "run this test as an unprivileged user"
        );
        let (fixture, mut broker, root, _digest) = registered_orphan_fixture();
        let healthy = root.parent().unwrap().join("z-healthy-root");
        let healthy_origin = fixture.path().join("healthy-origin");
        std::fs::create_dir(&healthy).unwrap();
        std::fs::create_dir(&healthy_origin).unwrap();
        let marker = crate::broker::WorktreeRootMarker {
            schema_version: crate::broker::WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: "healthy-historic-root".into(),
            repository_root: healthy_origin.clone(),
        };
        std::fs::write(
            healthy.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        std::fs::write(healthy.join("payload.txt"), "healthy payload").unwrap();
        crate::worktree_registry::register(
            &broker.host_operation_database_path().unwrap(),
            &healthy,
            &marker,
            crate::WorktreeRootSource::LibraryOverride,
        )
        .unwrap();
        std::fs::remove_dir(&healthy_origin).unwrap();
        let journal_path = broker.main_root().join(".aethyme/gc-journal.json");
        std::fs::remove_file(&journal_path).unwrap();
        let plan = broker.gc_plan().unwrap();
        assert_eq!(plan.orphans.len(), 2);
        assert_eq!(plan.orphans[0].worktree_root, root.to_string_lossy());
        let digest = plan.digest.clone();
        write_journal(&journal_path, &GcJournal::from(plan)).unwrap();
        let child = root.join("temporarily-unreadable");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("retained.txt"), "recoverable payload").unwrap();
        let restore = RestorePermissions(
            child.clone(),
            std::fs::metadata(&child).unwrap().permissions(),
        );
        std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o0)).unwrap();
        assert!(
            std::fs::read_dir(&child).is_err(),
            "permission denial must be real"
        );

        let blocked = broker.gc_apply(&digest).unwrap();
        assert!(!blocked.complete, "{blocked:?}");
        assert!(blocked.recovery_action.is_some());
        assert_eq!(
            blocked.orphans_removed,
            vec![healthy.to_string_lossy().into_owned()],
            "an unavailable root must not starve the healthy candidate behind it"
        );
        assert!(!healthy.exists());
        assert!(
            blocked
                .failures
                .iter()
                .any(|reason| reason.contains("retained in journal"))
        );
        assert_eq!(
            load_journal(&journal_path)
                .unwrap()
                .unwrap()
                .remaining_orphans
                .len(),
            1
        );
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());

        drop(restore);
        assert_eq!(
            std::fs::read_to_string(child.join("retained.txt")).unwrap(),
            "recoverable payload"
        );
        let completed = broker.gc_apply(&digest).unwrap();
        assert!(completed.complete, "{completed:?}");
        assert_eq!(
            completed.orphans_removed,
            vec![
                healthy.to_string_lossy().into_owned(),
                root.to_string_lossy().into_owned()
            ]
        );
        assert!(!root.exists());
        assert!(!journal_path.exists());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn a_legacy_orphan_authorization_is_revoked_before_fresh_review_can_remove_it() {
        let (_fixture, mut broker, root, digest) = registered_orphan_fixture();
        let path = broker.main_root().join(".aethyme/gc-journal.json");
        let mut journal = load_journal(&path).unwrap().unwrap();
        journal.remaining_orphans[0].ownership_fingerprint = None;
        write_journal(&path, &journal).unwrap();
        let revoked = broker.gc_apply(&digest).unwrap();
        assert!(revoked.complete);
        assert!(revoked.orphans_removed.is_empty());
        assert!(
            revoked
                .failures
                .iter()
                .any(|reason| reason.contains("legacy orphan authorization revoked"))
        );
        assert!(root.join("retained.txt").is_file());
        assert!(
            !path.exists(),
            "revoked authorization must not trap later reviewed plans"
        );
        let fresh = broker.gc_plan().unwrap();
        assert_eq!(fresh.orphans.len(), 1);
        let applied = broker.gc_apply(&fresh.digest).unwrap();
        assert!(applied.complete);
        assert_eq!(
            applied.orphans_removed,
            vec![root.to_string_lossy().into_owned()]
        );
        assert!(!root.exists());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn changed_orphan_evidence_revokes_only_the_old_authorization_and_allows_fresh_review() {
        let (_fixture, mut broker, root, digest) = registered_orphan_fixture();
        let marker = crate::worktree_registry::read_marker(&root).unwrap();
        std::fs::create_dir(&marker.repository_root).unwrap();
        let revoked = broker.gc_apply(&digest).unwrap();
        assert!(revoked.complete);
        assert!(revoked.orphans_removed.is_empty());
        assert!(
            revoked
                .failures
                .iter()
                .any(|reason| reason.contains("authorization revoked"))
        );
        assert!(root.join("retained.txt").is_file());
        std::fs::remove_dir(&marker.repository_root).unwrap();
        let fresh = broker.gc_plan().unwrap();
        assert_eq!(fresh.orphans.len(), 1);
        assert_eq!(
            fresh.orphans[0].worktree_root,
            root.to_string_lossy().as_ref()
        );
        // Authorization digests describe the candidate set, not the time of
        // review. Restoring the same origin absence can reproduce that set.
        // The revoked journal must be retired so a newly reviewed plan is
        // independently revalidated instead of resuming stale authority.
        assert!(!broker.main_root().join(".aethyme/gc-journal.json").exists());
        let applied = broker.gc_apply(&fresh.digest).unwrap();
        assert!(applied.complete);
        assert!(!root.exists());
    }

    fn session_history_fixture(count: i64) -> (tempfile::TempDir, crate::BrokerStore) {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::BrokerStore::open(&temp.path().join("history.db")).unwrap();
        let mut conn = rusqlite::Connection::open(store.db_path()).unwrap();
        let tx = conn.transaction().unwrap();
        for id in 1..=count {
            tx.execute(
                "INSERT INTO sessions (id, worktree_path, branch, origin, status,
                 cleanup_state, created_at, updated_at, last_activity_at)
                 VALUES (?1, ?2, 'fixture', 'spawned', 'cleaned', 'closed', 1, 1, 1)",
                rusqlite::params![id, format!("/synthetic/history/{id}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        (temp, store)
    }

    #[test]
    fn session_history_pages_resume_wrap_and_scope_without_duplicates() {
        let (_temp, store) = session_history_fixture(1799);
        for cursor in [0, 3, 32, 1759, 1799, 9999] {
            let mut pages = ClosedSessionPages::new(cursor, None);
            let mut actual = Vec::new();
            loop {
                let page = pages.next_page(&store).unwrap();
                assert!(page.len() <= 32);
                if page.is_empty() {
                    break;
                }
                actual.extend(page.into_iter().map(|session| session.id));
            }
            let expected = (1..=1799)
                .filter(|id| *id > cursor)
                .chain((1..=1799).filter(|id| *id <= cursor))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "cursor {cursor}");
            assert!(pages.next_page(&store).unwrap().is_empty());
        }
        let mut scoped = ClosedSessionPages::new(0, Some(1759));
        assert_eq!(
            scoped
                .next_page(&store)
                .unwrap()
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            vec![1759]
        );
        assert!(scoped.next_page(&store).unwrap().is_empty());
        assert!(
            ClosedSessionPages::new(0, Some(9999))
                .next_page(&store)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn session_history_expired_sweep_retains_off_page_recovery_authority() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nartifact_sweep_budget_ms = 0\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();
        let mut conn = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        let remembered_root = root.join("remembered-closed-worktree");
        let remembered = remembered_root.join("target");
        std::fs::create_dir_all(&remembered).unwrap();
        std::fs::write(remembered.join("preserve"), b"recovery evidence").unwrap();
        let tx = conn.transaction().unwrap();
        for id in 1..=1799 {
            let path = if id == 1500 {
                remembered_root.clone()
            } else {
                root.join(format!("absent-{id}"))
            };
            tx.execute(
                "INSERT INTO sessions (id, worktree_path, branch, origin, status,
                 cleanup_state, created_at, updated_at, last_activity_at)
                 VALUES (?1, ?2, 'fixture', 'spawned', 'cleaned', 'closed', 1, 1, 1)",
                rusqlite::params![id, path.to_string_lossy()],
            )
            .unwrap();
        }
        tx.execute(
            "UPDATE sessions SET repository_schema = -1 WHERE id = 33",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
        assert!(
            broker
                .artifact_sweep_budget_roots(ArtifactSweepScope::AutomaticContinuation)
                .contains(&remembered_root),
            "pressure roots must not decode unrelated session metadata"
        );
        let remembered_key = remembered.to_string_lossy().into_owned();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_INTERRUPTED_KEY, &remembered_key)
            .unwrap();
        let policy = RetentionPolicy {
            artifact_sweep_budget_ms: 5000,
            ..Default::default()
        };
        let expired = Instant::now() - Duration::from_millis(1);
        let (outcome, _) = broker
            .sweep_artifacts_autonomously_with_test_deadlines(
                &policy,
                ArtifactSweepScope::AutomaticContinuation,
                expired,
                expired,
            )
            .expect("expired sweep must not decode the second history page");
        assert!(!outcome.complete);
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some("1")
        );
        assert_eq!(
            decode_interrupted_paths(
                broker
                    .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                    .unwrap()
                    .as_deref()
            )
            .unwrap(),
            vec![remembered_key]
        );
        assert_eq!(
            std::fs::read(remembered.join("preserve")).unwrap(),
            b"recovery evidence"
        );
    }

    #[test]
    fn session_history_lap_does_not_chase_newly_closed_tail_rows() {
        let (_temp, store) = session_history_fixture(64);
        let conn = rusqlite::Connection::open(store.db_path()).unwrap();
        let mut pages = ClosedSessionPages::new(3, None);
        let mut actual = pages
            .next_page(&store)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect::<Vec<_>>();
        assert_eq!(actual, (4..=35).collect::<Vec<_>>());
        conn.execute(
            "INSERT INTO sessions (id, worktree_path, branch, origin, status,
             cleanup_state, created_at, updated_at, last_activity_at)
             VALUES (65, '/synthetic/history/65', 'fixture', 'spawned', 'cleaned', 'closed', 1, 1, 1)", [],
        ).unwrap();
        loop {
            let page = pages.next_page(&store).unwrap();
            if page.is_empty() {
                break;
            }
            actual.extend(page.into_iter().map(|s| s.id));
        }
        assert_eq!(actual, (4..=64).chain(1..=3).collect::<Vec<_>>());
        assert_eq!(
            ClosedSessionPages::new(64, None)
                .next_page(&store)
                .unwrap()
                .first()
                .unwrap()
                .id,
            65
        );
    }

    #[test]
    fn session_history_first_page_does_not_decode_later_rows() {
        let (_temp, store) = session_history_fixture(1799);
        let conn = rusqlite::Connection::open(store.db_path()).unwrap();
        conn.execute(
            "UPDATE sessions SET repository_schema = -1 WHERE id = 33",
            [],
        )
        .unwrap();
        let mut pages = ClosedSessionPages::new(0, None);
        let first = pages
            .next_page(&store)
            .expect("later history must not be decoded eagerly");
        assert_eq!(first.len(), 32);
        assert_eq!(first.first().unwrap().id, 1);
        assert_eq!(first.last().unwrap().id, 32);
        assert!(
            pages.next_page(&store).is_err(),
            "the malformed second page is not silently ignored"
        );
    }

    #[test]
    fn session_history_protection_includes_live_and_off_page_roots() {
        let (_temp, store) = session_history_fixture(1799);
        let conn = rusqlite::Connection::open(store.db_path()).unwrap();
        conn.execute(
            "UPDATE sessions SET status = 'active', cleanup_state = 'open' WHERE id > 1600",
            [],
        )
        .unwrap();
        assert_eq!(store.live_session_worktrees().unwrap().len(), 199);
        assert!(
            store
                .live_session_worktrees()
                .unwrap()
                .contains(&(1799, "/synthetic/history/1799".to_owned()))
        );
        let roots = store.session_worktree_paths().unwrap();
        assert_eq!(roots.len(), 1799);
        assert!(roots.contains(&"/synthetic/history/1500".to_owned()));
        let mut pages = ClosedSessionPages::new(1599, None);
        assert_eq!(
            pages
                .next_page(&store)
                .unwrap()
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            vec![1600]
        );
        assert_eq!(pages.next_page(&store).unwrap().first().unwrap().id, 1);
    }

    #[test]
    fn session_history_contract_backfill_selects_only_pending_live_rows() {
        let (_temp, store) = session_history_fixture(100);
        let conn = rusqlite::Connection::open(store.db_path()).unwrap();
        conn.execute("UPDATE sessions SET status = 'active', cleanup_state = 'open' WHERE id IN (1, 99, 100)", []).unwrap();
        conn.execute("UPDATE sessions SET deployment_state_digest = 'recorded', aethyme_version = 'fixture' WHERE id IN (1, 100)", []).unwrap();
        assert_eq!(
            store
                .sessions_missing_repository_contract()
                .unwrap()
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            vec![99]
        );
        conn.execute(
            "UPDATE sessions SET deployment_state_digest = NULL WHERE id = 100",
            [],
        )
        .unwrap();
        assert_eq!(
            store
                .sessions_missing_repository_contract()
                .unwrap()
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            vec![99, 100]
        );
    }

    #[test]
    fn interrupted_path_queue_reads_legacy_values_and_stays_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp
            .path()
            .join("first/target")
            .to_string_lossy()
            .into_owned();
        let second = tmp
            .path()
            .join("second/target")
            .to_string_lossy()
            .into_owned();

        assert_eq!(
            decode_interrupted_paths(Some(&first)).unwrap(),
            vec![first.clone()]
        );
        let encoded = encode_interrupted_paths(&[first.clone(), second.clone()]).unwrap();
        assert!(encoded.starts_with('['));
        assert_eq!(
            decode_interrupted_paths(Some(&encoded)).unwrap(),
            vec![first.clone(), second]
        );
        assert_eq!(
            encode_interrupted_paths(std::slice::from_ref(&first)).as_deref(),
            Some(first.as_str())
        );
        assert!(decode_interrupted_paths(Some("relative/target")).is_err());

        let too_many = (0..=ARTIFACT_SWEEP_INTERRUPTED_PATHS_MAX)
            .map(|index| {
                tmp.path()
                    .join(format!("{index}/target"))
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert!(encode_interrupted_paths(&too_many).is_none());
        let oversized = format!(
            "{}/{}",
            tmp.path().display(),
            "x".repeat(ARTIFACT_SWEEP_INTERRUPTED_VALUE_MAX_BYTES)
        );
        assert!(decode_interrupted_paths(Some(&oversized)).is_err());
    }

    #[test]
    fn an_interrupted_path_blocks_completion_without_starving_other_closed_caches() {
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
            "[retention]\nclosed_worktrees_days = 30\nclosed_worktree_grace_hours = 0\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();

        let mut make_closed = |label: &str| {
            let session = broker.start_worktree(label, None).unwrap();
            let worktree = PathBuf::from(&session.worktree_path);
            std::fs::write(worktree.join("landed.txt"), format!("{label}\n")).unwrap();
            git(&worktree, &["add", "landed.txt"]);
            git(&worktree, &["commit", "-qm", label]);
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
            std::fs::write(cache.join("debug/build.bin"), vec![b'x'; 128]).unwrap();
            (session.id, cache)
        };

        let (first_id, first_cache) = make_closed("first closed cache");
        let (later_id, later_cache) = make_closed("later closed cache");
        assert!(first_id < later_id);
        let active = broker
            .start_worktree("active-marker-blocker", None)
            .unwrap();
        let blocker = PathBuf::from(&active.worktree_path).join("target");
        std::fs::create_dir_all(&blocker).unwrap();
        std::fs::write(blocker.join("keep"), b"active\n").unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 30\nclosed_worktree_grace_hours = 0\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_REQUESTED_KEY, "7")
            .unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY, "3")
            .unwrap();
        broker
            .artifact_sweep_meta_set(ARTIFACT_SWEEP_INTERRUPTED_KEY, blocker.to_str().unwrap())
            .unwrap();
        let policy = load_retention_policy(root).unwrap();
        let mut pass_complete = true;

        broker
            .run_artifact_sweep_recovery_worker_loop(
                &policy,
                7,
                Some(1),
                Some(Duration::from_secs(10)),
                |broker, policy, remaining| {
                    let deadline = remaining.map(|remaining| Instant::now() + remaining);
                    let result = broker.sweep_artifacts_autonomously_with_deadline(
                        policy,
                        ArtifactSweepScope::AutomaticContinuation,
                        deadline,
                    );
                    if let Ok((outcome, _)) = &result {
                        pass_complete = outcome.complete;
                    }
                    result
                },
            )
            .unwrap();

        assert!(
            !pass_complete,
            "an unresolved queue entry withholds completion"
        );
        assert!(!first_cache.exists());
        assert!(
            !later_cache.exists(),
            "unrelated closed caches still make progress"
        );
        assert!(blocker.join("keep").exists(), "a live root stays untouched");
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_INTERRUPTED_KEY)
                .unwrap()
                .as_deref(),
            Some(blocker.to_str().unwrap()),
            "the unresolved path is preserved for later revalidation"
        );
        assert_eq!(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_RECOVERY_COMPLETED_KEY)
                .unwrap()
                .as_deref(),
            Some("3"),
            "the worker must not advance generation on an incomplete cache lap"
        );
        let blocked: serde_json::Value = serde_json::from_str(
            broker
                .artifact_sweep_meta_get(ARTIFACT_SWEEP_BLOCKED_PATHS_KEY)
                .unwrap()
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert!(blocked.as_array().unwrap().len() <= ARTIFACT_SWEEP_BLOCKED_DIAGNOSTICS_MAX);
        assert!(
            blocked
                .as_array()
                .unwrap()
                .iter()
                .any(|item| { item["reason"] == "live_session" && item["path"].is_string() })
        );
        assert!(test_worker_retry_pending(&mut broker, now_ms()).unwrap());
    }

    #[test]
    fn an_incomplete_large_worktree_does_not_starve_later_closed_sessions() {
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
            "[retention]\nclosed_worktrees_days = 30\nclosed_worktree_grace_hours = 0\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();

        let mut make_closed = |label: &str| {
            let session = broker.start_worktree(label, None).unwrap();
            let worktree = PathBuf::from(&session.worktree_path);
            std::fs::write(worktree.join("landed.txt"), format!("{label}\n")).unwrap();
            git(&worktree, &["add", "landed.txt"]);
            git(&worktree, &["commit", "-qm", label]);
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
            (session.id, worktree, cache)
        };

        let (large_id, large_worktree, large_cache) = make_closed("large first session");
        for index in 0..32 {
            std::fs::create_dir(large_worktree.join(format!("unclassified-{index:02}"))).unwrap();
        }
        let (later_id, later_worktree, later_cache) = make_closed("later session");
        assert!(large_id < later_id);
        assert!(large_cache.is_dir());
        assert!(later_cache.is_dir());
        let later_session = broker.store().session(later_id).unwrap();
        assert!(broker.is_broker_owned_worktree(&later_session, &later_worktree));
        assert!(
            broker
                .is_broker_owned_worktree_checked(&later_session, &later_worktree)
                .unwrap()
        );
        assert!(
            !broker
                .is_broker_owned_worktree_checked(&later_session, root)
                .unwrap()
        );
        let expired_ownership = {
            let _deadline = crate::git::limit_git_until(Instant::now() - Duration::from_millis(1));
            broker.is_broker_owned_worktree_checked(&later_session, &later_worktree)
        };
        assert!(matches!(
            expired_ownership,
            Err(crate::GitError::TimedOut { .. })
        ));
        let later_checkout = GitRepo::discover(&later_worktree).unwrap();
        assert!(later_checkout.path_is_ignored_checked("target").unwrap());

        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 30\nclosed_worktree_grace_hours = 0\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();
        let policy = load_retention_policy(root).unwrap();
        let (outcome, _) = broker
            .sweep_artifacts_autonomously_with_test_discovery_budget(
                &policy,
                ArtifactSweepScope::AutomaticContinuation,
                16,
            )
            .unwrap();

        assert!(
            !outcome.complete,
            "the bounded first worktree remains an explicit incomplete pass"
        );
        let cursor = broker
            .artifact_sweep_meta_get(ARTIFACT_SWEEP_CURSOR_KEY)
            .unwrap();
        let expected_cursor = later_id.to_string();
        assert_eq!(
            cursor.as_deref(),
            Some(expected_cursor.as_str()),
            "the incomplete first worktree must not prevent visiting the later session"
        );
        assert!(
            !later_cache.exists(),
            "later closed sessions still get scanned after an earlier worktree exceeds the entry budget; reclaimed={}, cursor={cursor:?}",
            outcome.directories_reclaimed
        );
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
        let bounded = collect_artifact_dirs_bounded(
            root,
            &[],
            Instant::now() + Duration::from_secs(1),
            ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET,
        );
        assert!(
            !bounded.complete,
            "a depth cap must not be reported as a complete cache lap"
        );
    }

    #[test]
    fn bounded_collection_reports_wide_tree_exhaustion_as_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for index in 0..32 {
            dir(root, &format!("wide-{index:02}"));
        }

        let discovery =
            collect_artifact_dirs_bounded(root, &[], Instant::now() + Duration::from_secs(1), 7);

        assert!(
            !discovery.complete,
            "unvisited siblings must keep the lap open"
        );
        assert_eq!(discovery.entries_examined, 7);
    }

    #[test]
    fn bounded_collection_reports_an_expired_deadline_as_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        dir(tmp.path(), "workspace");

        let discovery = collect_artifact_dirs_bounded(
            tmp.path(),
            &[],
            Instant::now() - Duration::from_millis(1),
            ARTIFACT_SWEEP_DISCOVERY_ENTRY_BUDGET,
        );

        assert!(!discovery.complete);
        assert_eq!(discovery.entries_examined, 0);
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
        std::fs::create_dir(tmp.path().join("rust")).unwrap();
        let missing = tmp.path().join("rust/target");
        assert_eq!(
            remove_condemned_tree(&missing, Some("CACHEDIR.TAG"), None).unwrap(),
            TreeRemoval::Complete
        );
    }

    #[test]
    fn pinned_removal_handles_nested_trees_and_missing_leaves() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = dir(tmp.path(), "target/debug");
        std::fs::write(nested.join(".DS_Store"), "finder\n").unwrap();

        assert_eq!(
            remove_condemned_tree(&nested, None, None).unwrap(),
            TreeRemoval::Complete
        );
        assert!(!nested.exists());
        assert_eq!(
            remove_condemned_tree(&tmp.path().join("target/absent"), None, None).unwrap(),
            TreeRemoval::Complete
        );
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
