//! Background full measurement of a repository's shared preparation cache.
//!
//! `submit prepare` measures the shared store against `[shared_cache]
//! max_bytes` while it holds the cache lease, and that inventory stops at a
//! time or entry limit. When it stops with the bytes it saw still within
//! budget it is inconclusive and the store is kept. A store too large to
//! inventory within those limits, whose first part happens to be under
//! budget, was therefore never measured at all: it stayed inconclusive for
//! ever and could grow past its budget without rotating, with a status notice
//! as the only signal.
//!
//! This module closes that gap without touching the foreground limits:
//!
//! - **Queue.** The first inconclusive check queues a full measurement of that
//!   cache root (one durable request per repository, coalesced). The first,
//!   not the third: the foreground check repeats at most hourly, so waiting
//!   for the notice threshold would leave an unknown store growing for hours,
//!   and the measurement costs nothing on the `submit prepare` path. The
//!   notice keeps its threshold as the escalation for when the background
//!   measurement itself cannot finish.
//! - **Measure.** The detached `gc` artifact worker runs it after its own
//!   lanes and after releasing the repository worker lock, at background CPU
//!   and I/O priority, one at a time per host (a host-wide advisory lock
//!   beside the deleter lock; without a host state directory the walk is
//!   skipped, never run unserialised), paced by the unattended pacer: a scan
//!   rate limit, memory pressure stops it, load throttles it. The request is
//!   marked running for the walk's duration (with the walker's pid), so no
//!   second worker starts it and nothing overwrites it; a marker older than
//!   the walk deadline plus slack, or whose process is gone, is taken over.
//!   The walk is the same streaming, descriptor-relative, no-follow walk the
//!   foreground uses (memory bounded by the directory depth), with no entry
//!   limit, a deadline, and an early stop once the bytes it has seen pass the
//!   budget plus [`BACKGROUND_MARGIN_PERCENT`].
//! - **No lease.** The cache lease makes the foreground inventory exact and
//!   keeps rotation and recovery away from running steps. The measurement
//!   holds no lease, so it never makes a `prepare` wait. Entries that vanish
//!   or are replaced while it walks are skipped and counted (a running step
//!   may be writing the store), and a step that unpacks into the store can
//!   make it count transient or renamed bytes, so its total is an estimate.
//!   Three guards keep an estimate from rotating a healthy store: over budget
//!   means over the budget plus a margin; a rotation needs two consecutive
//!   over-budget walks of the same store (the second queued after a short
//!   delay; a within-budget, in-margin or interrupted walk in between resets
//!   the count); and a rotation or root change during a walk is caught by
//!   comparing the root's and the store's identity before and after, which
//!   discards the walk. A walk that skipped entries or overlapped a running
//!   `prepare` is marked `concurrent_activity`. The rotation itself still
//!   runs in the foreground, under the lease, through the existing journalled
//!   path and its identity proofs.
//! - **Use.** The next foreground budget check that finds a result bound to
//!   the same root and store identity, within the trust the result was given,
//!   decides from it instead of running its own inventory: a confirmed over
//!   budget rotates, within budget is conclusive. Trust scales with headroom:
//!   the time until the budget at the growth rate between the last two
//!   results, clamped to 1 to 24 hours, or by the share of the budget used
//!   when there is one result. A used result queues its refresh only in the
//!   last quarter of that trust.
//! - **Restart, not resume.** An interrupted walk is restarted from scratch
//!   after a back-off (doubling for repeated deadline or refused walks of the
//!   same store, up to a day). Directory order is not stable across opens and
//!   the tree changes between runs, so a resumed partial sum would add up two
//!   different snapshots; the walk reads only metadata, so a restart is cheap.
//!
//! The walk holds open directory descriptors on the cache volume while it
//! runs, so an external volume cannot be ejected until it ends (at most the
//! deadline). Killing the worker is safe: the request stays durable and the
//! running marker is taken over once its process is gone. No broker signal
//! asks gc workers to stop, and nothing waits for them to exit.
//!
//! Both records live in the repository's broker metadata (never under
//! `.aethyme/`, which the cleanliness gates watch) and carry no path: the
//! cache root is named by a digest, its identity by a digest of the
//! volume-aware directory identity.

use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    PreparationError, SHARED_CACHE_BUDGET_CHECK_INTERVAL_MS, SharedCacheInventory,
    SharedCacheInventoryInconclusive, now_ms, rotation_debt,
};
use crate::gc::unattended_pacing;
use crate::{Broker, BrokerOpError};

/// Broker metadata key of the pending measurement request (JSON, or `null`).
pub(crate) const REQUEST_KEY: &str = "preparation.shared_cache_measurement.request";
/// Broker metadata key of the last completed measurement (JSON, or `null`).
pub(crate) const RESULT_KEY: &str = "preparation.shared_cache_measurement.result";
pub(crate) const REQUEST_SCHEMA: &str = "aethyme.preparation-cache-measurement-request/v1";
pub(crate) const RESULT_SCHEMA: &str = "aethyme.preparation-cache-measurement/v1";
/// Shortest and longest trust a result gets. The shortest is the
/// budget-check interval, the trust a foreground measurement gets.
pub(crate) const MIN_TRUST_MS: i64 = SHARED_CACHE_BUDGET_CHECK_INTERVAL_MS;
pub(crate) const MAX_TRUST_MS: i64 = 24 * 60 * 60 * 1000;
/// Trust by share of the budget when no growth rate is known yet.
const TRUST_AT_HALF_MS: i64 = MAX_TRUST_MS;
const TRUST_AT_80_PERCENT_MS: i64 = 6 * 60 * 60 * 1000;
/// Background over budget means over the budget plus this many percent. A
/// lease-free walk can count bytes a running step is unpacking or renaming;
/// "within budget" still means at or under the budget itself, and a result
/// between the two decides nothing.
pub(crate) const BACKGROUND_MARGIN_PERCENT: u64 = 5;
/// Consecutive over-budget walks of one store needed before a rotation.
pub(crate) const OVER_BUDGET_CONFIRMATIONS: u32 = 2;
/// Delay before the confirming walk: long enough for a running step's
/// download or unpack to finish, short enough to rotate the same hour.
pub(crate) const CONFIRMATION_DELAY_MS: i64 = 2 * 60_000;
/// Back-off after an interrupted walk before a worker restarts it; repeated
/// deadline or refused walks of one store double it up to a day.
pub(crate) const RETRY_AFTER_INTERRUPTION_MS: i64 = 10 * 60_000;
pub(crate) const MAX_RETRY_AFTER_MS: i64 = 24 * 60 * 60 * 1000;
/// Back-off when another repository's measurement holds the host slot.
pub(crate) const RETRY_AFTER_HOST_BUSY_MS: i64 = 60_000;
/// Wall-clock backstop for one walk. At the scan rate limit it covers
/// 18 million entries; a store that takes longer is reported, not guessed.
pub(crate) const MAX_WALK_DURATION: Duration = Duration::from_secs(30 * 60);
/// A running marker older than the deadline plus this slack is stale.
pub(crate) const RUNNING_STALE_SLACK_MS: i64 = 10 * 60_000;
/// Scan rate limit for the walk, in entries a second. Ten times the
/// unattended unlink default (1,000/s): a metadata read writes nothing, wakes
/// no file-event or indexing daemon and dirties no memory, but each entry
/// still pulls a vnode and inode into the kernel caches, which this bounds.
/// A 5-million-entry store takes about 8.5 minutes, inside the deadline.
pub(crate) const SCAN_ENTRIES_PER_SECOND: u32 = 10_000;
/// Entries between two pacer checks (rate, memory, load).
pub(crate) const PACING_BATCH_ENTRIES: u64 = 4096;
/// Host-wide advisory lock: one measurement per machine at a time.
pub(crate) const HOST_LOCK_FILE: &str = "preparation-cache-measurement.lock";
const ROTATION_PROBE_MAX_ENTRIES: usize = 4096;
const ROTATION_PROBE_BUDGET: Duration = Duration::from_secs(2);
const CAS_ATTEMPTS: usize = 8;

/// The early-stop and rotation threshold of a background decision.
pub(crate) fn over_budget_threshold(budget: u64) -> u64 {
    let margin = u128::from(budget) * u128::from(BACKGROUND_MARGIN_PERCENT) / 100;
    u64::try_from(u128::from(budget) + margin).unwrap_or(u64::MAX)
}

/// Why a background measurement stopped without an answer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementInterruptionReason {
    /// Memory pressure outlasted the pacer's pause budget.
    MemoryPressure,
    /// The walk reached [`MAX_WALK_DURATION`].
    Deadline,
    /// The cache root or the store was rotated, replaced or remounted.
    RootChanged,
    /// A rotation is unfinished: the next `prepare` recovers it first.
    RotationPending,
    /// The cache root could not be identified or validated.
    RootUnavailable,
    /// The walk refused the tree (special file, filesystem crossing, I/O).
    ScanRefused,
}

impl MeasurementInterruptionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MemoryPressure => "memory_pressure",
            Self::Deadline => "deadline",
            Self::RootChanged => "root_changed",
            Self::RotationPending => "rotation_pending",
            Self::RootUnavailable => "root_unavailable",
            Self::ScanRefused => "scan_refused",
        }
    }

    fn counter(self) -> &'static str {
        match self {
            Self::MemoryPressure => "preparation_cache_measurement_interrupted_memory_pressure",
            Self::Deadline => "preparation_cache_measurement_interrupted_deadline",
            Self::RootChanged => "preparation_cache_measurement_interrupted_root_changed",
            Self::RotationPending => "preparation_cache_measurement_interrupted_rotation_pending",
            Self::RootUnavailable => "preparation_cache_measurement_interrupted_root_unavailable",
            Self::ScanRefused => "preparation_cache_measurement_interrupted_scan_refused",
        }
    }

    /// A changed root makes the request obsolete: the store it named is
    /// gone, and the next inconclusive check queues the current one.
    fn drops_request(self) -> bool {
        matches!(self, Self::RootChanged)
    }

    /// Failures that repeat for the same store until it changes: back off
    /// exponentially instead of re-walking it every ten minutes.
    fn backs_off(self) -> bool {
        matches!(self, Self::Deadline | Self::ScanRefused)
    }
}

/// What an interrupted walk had seen when it stopped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedCacheMeasurementInterruption {
    pub reason: MeasurementInterruptionReason,
    /// The walk's own refusal reason; never a path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub at_ms: i64,
    pub attempt: u64,
    pub entries: u64,
    pub bytes: u64,
    pub duration_ms: u64,
    pub pacer_memory_pause_ms: u64,
    pub pacer_load_pause_ms: u64,
}

/// A queued background measurement of one repository's shared store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedCacheMeasurementRequest {
    pub schema: String,
    /// Repository identity: the store's directory name under the cache root.
    pub repository: String,
    /// Digest naming the cache root, as in the budget-check stamp key.
    pub cache_root_key: String,
    /// Digest of the cache root's directory identity when queued.
    #[serde(default)]
    pub root_identity: Option<String>,
    pub budget_bytes: u64,
    /// The foreground outcome that queued it (`inconclusive_time_limit`,
    /// `inconclusive_entry_limit`), `refresh`, or `confirm_over_budget`.
    pub reason: String,
    pub queued_at_ms: i64,
    /// How many checks asked for it while it was pending.
    #[serde(default)]
    pub times_queued: u64,
    /// Walks started for it; more than one means it was restarted.
    #[serde(default)]
    pub attempts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_interruption: Option<SharedCacheMeasurementInterruption>,
    /// Set while a worker walks for this request: no other worker starts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_since_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_pid: Option<u32>,
    /// Consecutive deadline or refused walks of `failure_store_identity`.
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_store_identity: Option<String>,
}

impl SharedCacheMeasurementRequest {
    /// Whether a live worker is walking for it now.
    fn running(&self, now: i64) -> bool {
        let Some(since) = self.running_since_ms else {
            return false;
        };
        let deadline_ms = i64::try_from(MAX_WALK_DURATION.as_millis()).unwrap_or(i64::MAX);
        let fresh = now.saturating_sub(since) < deadline_ms.saturating_add(RUNNING_STALE_SLACK_MS);
        fresh
            && self
                .running_pid
                .is_none_or(|pid| crate::blockers::process_alive(i64::from(pid)))
    }

    fn due(&self, now: i64) -> bool {
        !self.running(now) && self.retry_after_ms.is_none_or(|after| after <= now)
    }
}

/// The answer a completed measurement gives.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementOutcome {
    /// Complete, at or under the budget.
    WithinBudget,
    /// Over the budget plus the margin (`bytes` may be a lower bound).
    OverBudget,
    /// Over the budget but within the margin: decides nothing.
    WithinMargin,
}

impl MeasurementOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WithinBudget => "within_budget",
            Self::OverBudget => "over_budget",
            Self::WithinMargin => "within_margin",
        }
    }
}

fn default_trusted_for_ms() -> i64 {
    MIN_TRUST_MS
}

/// The last completed background measurement. Interrupted walks are kept in
/// the request's `last_interruption` and never replace this record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedCacheMeasurement {
    pub schema: String,
    pub repository: String,
    pub cache_root_key: String,
    /// Digest of the cache root's directory identity while it was walked.
    #[serde(default)]
    pub root_identity: Option<String>,
    /// Digest of the store directory's identity; null when it was absent.
    #[serde(default)]
    pub store_identity: Option<String>,
    pub budget_bytes: u64,
    pub outcome: MeasurementOutcome,
    /// The walk reached the end of the store. False when it stopped early
    /// because the bytes it had seen passed the threshold (`bytes` is then a
    /// lower bound).
    pub complete: bool,
    pub bytes: u64,
    pub entries: u64,
    /// Entries skipped because they vanished or were replaced mid-walk.
    #[serde(default)]
    pub skipped_entries: u64,
    /// The walk skipped entries or overlapped a `prepare` holding the cache.
    #[serde(default)]
    pub concurrent_activity: bool,
    /// Consecutive over-budget walks of this store, this one included.
    #[serde(default)]
    pub over_budget_walks: u32,
    /// An over-budget result confirmed by enough consecutive walks: only
    /// then does a budget check rotate from it.
    #[serde(default)]
    pub over_budget_confirmed: bool,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub duration_ms: u64,
    pub entries_per_second: u64,
    pub bytes_per_second: u64,
    pub pacer_memory_pause_ms: u64,
    pub pacer_load_pause_ms: u64,
    #[serde(default)]
    pub pacer_scan_limiter_sleep_ms: u64,
    pub attempt: u64,
    /// How long a budget check may decide from this record, and why.
    #[serde(default = "default_trusted_for_ms")]
    pub trusted_for_ms: i64,
    #[serde(default)]
    pub trust_reason: String,
    /// When a foreground budget check last decided from this record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_at_ms: Option<i64>,
    #[serde(default)]
    pub times_used: u64,
}

impl SharedCacheMeasurement {
    fn age_ms(&self, now: i64) -> i64 {
        now.saturating_sub(self.completed_at_ms)
    }

    fn fresh(&self, now: i64) -> bool {
        (0..self.trusted_for_ms.max(MIN_TRUST_MS)).contains(&self.age_ms(now))
    }

    /// Whether this record answers the budget question for `budget`: complete
    /// and at or under it, or confirmed over it plus the margin.
    fn decides(&self, budget: u64) -> bool {
        match self.outcome {
            MeasurementOutcome::WithinBudget => self.complete && self.bytes <= budget,
            MeasurementOutcome::OverBudget => {
                self.over_budget_confirmed && self.bytes > over_budget_threshold(budget)
            }
            MeasurementOutcome::WithinMargin => false,
        }
    }

    fn same_store(&self, other: &Self) -> bool {
        self.repository == other.repository
            && self.cache_root_key == other.cache_root_key
            && self.root_identity == other.root_identity
            && self.store_identity == other.store_identity
    }
}

/// How long a within-budget result is trusted, and why.
fn trust_for(
    previous: Option<&SharedCacheMeasurement>,
    current: &SharedCacheMeasurement,
) -> (i64, &'static str) {
    if current.outcome != MeasurementOutcome::WithinBudget {
        return (MIN_TRUST_MS, "not_within_budget");
    }
    let budget = current.budget_bytes;
    let headroom = budget.saturating_sub(current.bytes);
    if let Some(previous) = previous.filter(|previous| {
        previous.same_store(current)
            && previous.outcome == MeasurementOutcome::WithinBudget
            && previous.complete
            && previous.completed_at_ms < current.completed_at_ms
    }) {
        let elapsed = current.completed_at_ms - previous.completed_at_ms;
        let growth = current.bytes.saturating_sub(previous.bytes);
        if growth == 0 {
            return (MAX_TRUST_MS, "not_growing");
        }
        let until_budget =
            u128::from(headroom) * elapsed.unsigned_abs() as u128 / u128::from(growth);
        let until_budget = i64::try_from(until_budget).unwrap_or(i64::MAX);
        return (
            until_budget.clamp(MIN_TRUST_MS, MAX_TRUST_MS),
            "growth_rate",
        );
    }
    let used = u128::from(current.bytes) * 100;
    if used <= u128::from(budget) * 50 {
        (TRUST_AT_HALF_MS, "at_most_half_of_budget")
    } else if used <= u128::from(budget) * 80 {
        (TRUST_AT_80_PERCENT_MS, "at_most_80_percent_of_budget")
    } else {
        (MIN_TRUST_MS, "above_80_percent_of_budget")
    }
}

/// `submit prepare status --json`: the repository's background measurement.
#[derive(Debug, Clone, Serialize)]
pub struct SharedCacheMeasurementStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<SharedCacheMeasurementRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<SharedCacheMeasurement>,
    /// Whether `result` is within its trust for the next budget check; root
    /// and store identity are checked then.
    pub result_fresh: bool,
    /// Whether a worker is walking for `request` now.
    pub running: bool,
}

/// The background measurement a budget check decided from.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SharedCacheMeasurementUse {
    pub outcome: MeasurementOutcome,
    pub bytes: u64,
    pub entries: u64,
    pub complete: bool,
    pub completed_at_ms: i64,
    pub age_ms: i64,
}

/// The prepare report's `shared_cache_background_measurement`.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SharedCacheBackgroundMeasurementReport {
    /// Present when this check's budget decision came from a background
    /// measurement instead of a foreground inventory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<SharedCacheMeasurementUse>,
    /// Why this prepare queued a background measurement, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<String>,
}

/// What one worker pass of the measurement lane did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MeasurementLaneOutcome {
    NotDue,
    /// Another worker is walking for the request; nothing was written.
    AlreadyRunning,
    HostBusy,
    /// No host state directory holds the measurement lock: skipped.
    HostLockUnavailable,
    Completed(SharedCacheMeasurement),
    Interrupted(SharedCacheMeasurementInterruption),
}

/// The digest naming a cache root, shared with the budget-check stamp key.
pub(crate) fn cache_root_key(cache_root: &Path) -> String {
    let digest = super::hash_text(&cache_root.to_string_lossy());
    digest[..12].to_string()
}

/// Digest of a directory's volume-aware identity (device, inode and
/// filesystem identifier); `None` when it is absent or cannot be identified.
pub(crate) fn identity_digest(path: &Path) -> Option<String> {
    let encoded = rotation_debt::DirectoryIdentity::capture(path)
        .and_then(|identity| identity.encode())
        .ok()?;
    let digest = format!("{:x}", Sha256::digest(&encoded));
    Some(digest[..32].to_string())
}

fn parse<T: serde::de::DeserializeOwned>(raw: Option<&str>) -> Option<T> {
    raw.and_then(|value| serde_json::from_str::<Option<T>>(value).ok().flatten())
}

/// The inventory a foreground budget check uses in place of its own, when
/// `measurement` still describes the store at `store`. Called with the lease
/// held, after rotation recovery, so a store rotated since the walk has a
/// different identity and the measurement is ignored.
pub(super) fn inventory_for_current_store(
    measurement: &SharedCacheMeasurement,
    store: &Path,
    budget: u64,
) -> Option<SharedCacheInventory> {
    if identity_digest(store) != measurement.store_identity {
        crate::phase_telemetry::add(
            "preparation_cache_measurement_ignored_store_changed",
            Duration::ZERO,
        );
        return None;
    }
    if !measurement.decides(budget) {
        return None;
    }
    Some(
        match (measurement.store_identity.is_some(), measurement.complete) {
            (false, _) => SharedCacheInventory::Absent,
            (true, true) => SharedCacheInventory::Complete {
                bytes: measurement.bytes,
            },
            (true, false) => SharedCacheInventory::OverBudget {
                observed_bytes_lower_bound: measurement.bytes,
            },
        },
    )
}

impl Broker {
    fn read_measurement_request(&self) -> (Option<String>, Option<SharedCacheMeasurementRequest>) {
        let raw = self.store_ref().meta_get(REQUEST_KEY).ok().flatten();
        let request = parse(raw.as_deref());
        (raw, request)
    }

    fn read_measurement_result(&self) -> (Option<String>, Option<SharedCacheMeasurement>) {
        let raw = self.store_ref().meta_get(RESULT_KEY).ok().flatten();
        let result = parse(raw.as_deref());
        (raw, result)
    }

    /// The pending background measurement request, if any.
    pub(crate) fn shared_cache_measurement_request(&self) -> Option<SharedCacheMeasurementRequest> {
        self.read_measurement_request().1
    }

    /// The last completed background measurement, if any.
    pub(crate) fn shared_cache_measurement_result(&self) -> Option<SharedCacheMeasurement> {
        self.read_measurement_result().1
    }

    /// Whether a request is waiting, not running and not backing off. One
    /// metadata read, cheap enough for worker admission on every broker open.
    pub(crate) fn shared_cache_measurement_due(&self, now: i64) -> bool {
        self.shared_cache_measurement_request()
            .is_some_and(|request| request.due(now))
    }

    /// The status view, or `None` when nothing was ever queued or measured.
    pub(crate) fn shared_cache_measurement_status(&self) -> Option<SharedCacheMeasurementStatus> {
        let request = self.shared_cache_measurement_request();
        let result = self.shared_cache_measurement_result();
        if request.is_none() && result.is_none() {
            return None;
        }
        let now = now_ms();
        Some(SharedCacheMeasurementStatus {
            result_fresh: result.as_ref().is_some_and(|result| result.fresh(now)),
            running: request.as_ref().is_some_and(|request| request.running(now)),
            request,
            result,
        })
    }

    /// Drop both records: the store they describe was rotated away.
    pub(crate) fn forget_shared_cache_measurement(&mut self) {
        for key in [REQUEST_KEY, RESULT_KEY] {
            let written = self
                .store()
                .meta_set(key, "null")
                .map_err(|error| error.to_string());
            crate::warn_unrecorded("forget the shared-cache background measurement", written);
        }
    }

    /// A measurement of `repository`'s store under `cache_root` that is within
    /// its trust, bound to the root's current identity, and decides `budget`.
    /// The store's identity is checked later, under the lease.
    pub(crate) fn fresh_shared_cache_measurement(
        &self,
        repository: &str,
        cache_root: &Path,
        budget: u64,
        now: i64,
    ) -> Option<SharedCacheMeasurement> {
        let result = self.shared_cache_measurement_result()?;
        if result.repository != repository || result.cache_root_key != cache_root_key(cache_root) {
            return None;
        }
        if !result.fresh(now) || !result.decides(budget) {
            return None;
        }
        let root_identity = identity_digest(cache_root);
        if root_identity.is_none() || result.root_identity != root_identity {
            crate::phase_telemetry::add(
                "preparation_cache_measurement_ignored_root_changed",
                Duration::ZERO,
            );
            return None;
        }
        Some(result)
    }

    /// Queue (or coalesce into the pending) measurement of `repository`'s
    /// store under `cache_root`. Returns whether the request is durable. A
    /// coalesce keeps a running walk's marker and attempt, so the walk still
    /// clears or updates the request it started.
    pub(crate) fn queue_shared_cache_measurement(
        &mut self,
        repository: &str,
        cache_root: &Path,
        budget_bytes: u64,
        reason: &str,
    ) -> bool {
        let key = cache_root_key(cache_root);
        let root_identity = identity_digest(cache_root);
        for _ in 0..CAS_ATTEMPTS {
            let (raw, existing) = self.read_measurement_request();
            let now = now_ms();
            let (request, counter) = match existing {
                Some(mut pending)
                    if pending.repository == repository && pending.cache_root_key == key =>
                {
                    pending.times_queued = pending.times_queued.saturating_add(1);
                    pending.budget_bytes = budget_bytes;
                    pending.root_identity = root_identity.clone();
                    // A confirming walk keeps its purpose.
                    if pending.reason != "confirm_over_budget" {
                        pending.reason = reason.to_string();
                    }
                    (pending, "preparation_cache_measurement_coalesced")
                }
                _ => (
                    SharedCacheMeasurementRequest {
                        schema: REQUEST_SCHEMA.into(),
                        repository: repository.to_string(),
                        cache_root_key: key.clone(),
                        root_identity: root_identity.clone(),
                        budget_bytes,
                        reason: reason.to_string(),
                        queued_at_ms: now,
                        times_queued: 1,
                        attempts: 0,
                        retry_after_ms: None,
                        last_interruption: None,
                        running_since_ms: None,
                        running_pid: None,
                        consecutive_failures: 0,
                        failure_store_identity: None,
                    },
                    "preparation_cache_measurement_queued",
                ),
            };
            match self.replace_measurement_request(raw.as_deref(), Some(&request)) {
                Ok(true) => {
                    crate::phase_telemetry::add(counter, Duration::ZERO);
                    return true;
                }
                Ok(false) => continue,
                Err(error) => {
                    crate::warn_unrecorded(
                        "queue the shared-cache background measurement",
                        Err::<(), _>(error),
                    );
                    return false;
                }
            }
        }
        false
    }

    /// Compare-and-set the request; `None` writes `null` (no request).
    fn replace_measurement_request(
        &mut self,
        expected: Option<&str>,
        request: Option<&SharedCacheMeasurementRequest>,
    ) -> Result<bool, String> {
        let value = serde_json::to_string(&request).map_err(|error| error.to_string())?;
        self.store()
            .meta_compare_and_set(REQUEST_KEY, expected, &value)
            .map_err(|error| error.to_string())
    }

    /// The walker's final write: applied only while the request is still the
    /// one this walk marked running (same attempt and start), whatever a
    /// prepare coalesced into it meanwhile. Returns whether it was applied.
    fn finish_measurement_request(
        &mut self,
        attempt: u64,
        running_since_ms: i64,
        update: impl Fn(SharedCacheMeasurementRequest) -> Option<SharedCacheMeasurementRequest>,
    ) -> bool {
        for _ in 0..CAS_ATTEMPTS {
            let (raw, current) = self.read_measurement_request();
            let Some(current) = current.filter(|current| {
                current.attempts == attempt && current.running_since_ms == Some(running_since_ms)
            }) else {
                // Dropped by a rotation, or taken over as stale.
                return false;
            };
            let next = update(current).map(|mut next| {
                next.running_since_ms = None;
                next.running_pid = None;
                next
            });
            match self.replace_measurement_request(raw.as_deref(), next.as_ref()) {
                Ok(true) => return true,
                Ok(false) => continue,
                Err(error) => {
                    crate::warn_unrecorded(
                        "finish the shared-cache background measurement request",
                        Err::<(), _>(error),
                    );
                    return false;
                }
            }
        }
        false
    }

    /// After a foreground budget check: note a decision taken from a
    /// background measurement and queue what the outcome calls for.
    pub(crate) fn follow_up_shared_cache_budget_check(
        &mut self,
        outcome: Option<&str>,
        repository: &str,
        cache_root: &Path,
        budget_bytes: u64,
        used: Option<&SharedCacheMeasurement>,
    ) -> Option<SharedCacheBackgroundMeasurementReport> {
        let outcome = outcome?;
        let now = now_ms();
        if let Some(measurement) = used.filter(|_| outcome.ends_with("_background")) {
            self.note_shared_cache_measurement_used(now);
            let age_ms = measurement.age_ms(now);
            let decided_by = SharedCacheMeasurementUse {
                outcome: measurement.outcome,
                bytes: measurement.bytes,
                entries: measurement.entries,
                complete: measurement.complete,
                completed_at_ms: measurement.completed_at_ms,
                age_ms,
            };
            // A rotated store needs no refresh. A kept one is still too large
            // to measure in the foreground: refresh it in the last quarter of
            // its trust, so the next checks find a fresh result without a
            // full walk every hour.
            let refresh_due =
                age_ms.saturating_mul(4) >= measurement.trusted_for_ms.saturating_mul(3);
            let queued = (outcome == "within_budget_background"
                && refresh_due
                && self.queue_shared_cache_measurement(
                    repository,
                    cache_root,
                    budget_bytes,
                    "refresh",
                ))
            .then(|| "refresh".to_string());
            return Some(SharedCacheBackgroundMeasurementReport {
                decided_by: Some(decided_by),
                queued,
            });
        }
        if outcome.starts_with("inconclusive") {
            let queued = self
                .queue_shared_cache_measurement(repository, cache_root, budget_bytes, outcome)
                .then(|| outcome.to_string());
            return queued.map(|queued| SharedCacheBackgroundMeasurementReport {
                decided_by: None,
                queued: Some(queued),
            });
        }
        None
    }

    fn note_shared_cache_measurement_used(&mut self, now: i64) {
        let (Some(raw), Some(mut result)) = self.read_measurement_result() else {
            return;
        };
        result.used_at_ms = Some(now);
        result.times_used = result.times_used.saturating_add(1);
        let written = serde_json::to_string(&result)
            .map_err(|error| error.to_string())
            .and_then(|value| {
                self.store()
                    .meta_compare_and_set(RESULT_KEY, Some(&raw), &value)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            });
        crate::warn_unrecorded("record the shared-cache measurement use", written);
    }

    /// An interrupted walk breaks a run of over-budget walks: the next one
    /// must be confirmed again.
    fn reset_over_budget_confirmation(&mut self) {
        let (Some(raw), Some(mut result)) = self.read_measurement_result() else {
            return;
        };
        if result.outcome != MeasurementOutcome::OverBudget || result.over_budget_walks == 0 {
            return;
        }
        result.over_budget_walks = 0;
        result.over_budget_confirmed = false;
        let written = serde_json::to_string(&result)
            .map_err(|error| error.to_string())
            .and_then(|value| {
                self.store()
                    .meta_compare_and_set(RESULT_KEY, Some(&raw), &value)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            });
        crate::warn_unrecorded("reset the over-budget confirmation", written);
    }

    /// Update the repeated-inconclusive record after a background walk: a
    /// result that decides resets it (the status notice clears), anything
    /// else is noted in it so the notice says why it is still unmeasured.
    fn note_background_measurement_on_inconclusive_record(
        &mut self,
        settled: Option<&SharedCacheMeasurement>,
        note: String,
    ) {
        let Some(mut record) = self
            .store_ref()
            .meta_get(super::SHARED_CACHE_INVENTORY_INCONCLUSIVE_KEY)
            .ok()
            .flatten()
            .and_then(|value| {
                serde_json::from_str::<SharedCacheInventoryInconclusive>(&value).ok()
            })
        else {
            return;
        };
        if record.consecutive == 0 {
            return;
        }
        if let Some(settled) = settled {
            record.consecutive = 0;
            record.last_at_ms = settled.completed_at_ms;
            record.outcome = format!("{}_background", settled.outcome.as_str());
            record.entries_seen = settled.entries;
            record.bytes_seen = settled.bytes;
            record.elapsed_ms = settled.duration_ms;
        }
        record.background_measurement = Some(note);
        let written = serde_json::to_string(&record)
            .map_err(|error| error.to_string())
            .and_then(|value| {
                self.store()
                    .meta_set(super::SHARED_CACHE_INVENTORY_INCONCLUSIVE_KEY, &value)
                    .map_err(|error| error.to_string())
            });
        crate::warn_unrecorded("record the background measurement on the notice", written);
    }

    /// The measurement lane of the detached `gc` artifact worker. Runs at most
    /// one walk, never holds the cache lease, and leaves the request durable
    /// whenever it cannot finish.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn run_preparation_cache_measurement_lane(
        &mut self,
        host_lock_directory: Option<&Path>,
    ) -> Result<MeasurementLaneOutcome, BrokerOpError> {
        if !self.shared_cache_measurement_due(now_ms()) {
            return Ok(MeasurementLaneOutcome::NotDue);
        }
        // A root that can no longer be resolved is reported as changed.
        let bindings = self
            .preparation_cache_root_bindings_for_maintenance()
            .unwrap_or_default();
        let resource_db_path = self.storage_context().host_resource_db_path.clone();
        self.run_preparation_cache_measurement_lane_with(
            host_lock_directory,
            &bindings,
            resource_db_path.as_deref(),
        )
    }

    /// [`Self::run_preparation_cache_measurement_lane`] against explicit
    /// cache-root bindings and host resource database.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn run_preparation_cache_measurement_lane_with(
        &mut self,
        host_lock_directory: Option<&Path>,
        bindings: &[crate::broker::PreparationCacheRootBinding],
        resource_db_path: Option<&Path>,
    ) -> Result<MeasurementLaneOutcome, BrokerOpError> {
        self.run_preparation_cache_measurement_lane_hooked(
            host_lock_directory,
            bindings,
            resource_db_path,
            &mut |_| {},
        )
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn run_preparation_cache_measurement_lane_hooked(
        &mut self,
        host_lock_directory: Option<&Path>,
        bindings: &[crate::broker::PreparationCacheRootBinding],
        resource_db_path: Option<&Path>,
        before_open: &mut dyn FnMut(super::SharedCacheScanOpenTarget),
    ) -> Result<MeasurementLaneOutcome, BrokerOpError> {
        let now = now_ms();
        let (raw, Some(mut request)) = self.read_measurement_request() else {
            return Ok(MeasurementLaneOutcome::NotDue);
        };
        if request.running(now) {
            // Another worker walks for it: never write over its request.
            crate::phase_telemetry::add(
                "preparation_cache_measurement_already_running",
                Duration::ZERO,
            );
            return Ok(MeasurementLaneOutcome::AlreadyRunning);
        }
        if request.retry_after_ms.is_some_and(|after| after > now) {
            return Ok(MeasurementLaneOutcome::NotDue);
        }
        let took_over = request.running_since_ms.is_some();
        let _lane = crate::phase_telemetry::span("preparation_cache_measurement_lane");
        let _host_slot = match acquire_host_measurement_slot(host_lock_directory) {
            HostMeasurementSlot::Held(lock) => lock,
            busy_or_unavailable => {
                let (counter, retry, outcome) = match busy_or_unavailable {
                    HostMeasurementSlot::Busy => (
                        "preparation_cache_measurement_host_busy",
                        RETRY_AFTER_HOST_BUSY_MS,
                        MeasurementLaneOutcome::HostBusy,
                    ),
                    _ => (
                        "preparation_cache_measurement_host_lock_unavailable",
                        RETRY_AFTER_INTERRUPTION_MS,
                        MeasurementLaneOutcome::HostLockUnavailable,
                    ),
                };
                crate::phase_telemetry::add(counter, Duration::ZERO);
                request.retry_after_ms = Some(now.saturating_add(retry));
                let written = self.replace_measurement_request(raw.as_deref(), Some(&request));
                crate::warn_unrecorded(
                    "defer the shared-cache background measurement",
                    written.map(|_| ()),
                );
                return Ok(outcome);
            }
        };
        let running_since_ms = now_ms();
        request.attempts = request.attempts.saturating_add(1);
        request.retry_after_ms = None;
        request.running_since_ms = Some(running_since_ms);
        request.running_pid = Some(std::process::id());
        let attempt = request.attempts;
        if !self
            .replace_measurement_request(raw.as_deref(), Some(&request))
            .unwrap_or(false)
        {
            // Another worker or a prepare changed it meanwhile: next pass.
            return Ok(MeasurementLaneOutcome::NotDue);
        }
        if took_over {
            crate::phase_telemetry::add(
                "preparation_cache_measurement_took_over_stale",
                Duration::ZERO,
            );
        }
        crate::phase_telemetry::add(
            if attempt > 1 {
                "preparation_cache_measurement_restarted"
            } else {
                "preparation_cache_measurement_started"
            },
            Duration::ZERO,
        );
        let binding = bindings
            .iter()
            .find(|binding| cache_root_key(&binding.path) == request.cache_root_key);
        let pacing_before = unattended_pacing::current_summary(now_ms());
        let started = Instant::now();
        let run = match binding {
            None => Err(Interrupted::new(
                MeasurementInterruptionReason::RootChanged,
                Some("the cache root is no longer bound to this repository".into()),
            )),
            Some(binding) => match binding.validate() {
                Err(_) => Err(Interrupted::new(
                    MeasurementInterruptionReason::RootUnavailable,
                    Some("the captured cache root changed".into()),
                )),
                Ok(()) => measure_store(MeasureRequest {
                    cache_root: &binding.path,
                    binding_identity: binding.identity.as_deref(),
                    expected_root_identity: request.root_identity.as_deref(),
                    repository: &request.repository,
                    budget: request.budget_bytes,
                    resource_db_path,
                    max_elapsed: MAX_WALK_DURATION,
                    before_open,
                }),
            },
        };
        let duration = started.elapsed();
        let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let pacing_after = unattended_pacing::current_summary(now_ms());
        let pause = |pick: fn(&unattended_pacing::UnattendedPacingSummary) -> u64| {
            pacing_after
                .as_ref()
                .map_or(0, pick)
                .saturating_sub(pacing_before.as_ref().map_or(0, pick))
        };
        let memory_pause_ms = pause(|summary| summary.memory_pause_ms);
        let load_pause_ms = pause(|summary| summary.load_pause_ms);
        let scan_sleep_ms = pause(|summary| summary.scan_limiter_sleep_ms);
        crate::phase_telemetry::add("preparation_cache_measurement_walk", duration);
        crate::phase_telemetry::add(
            "preparation_cache_measurement_pacer_memory_pause",
            Duration::from_millis(memory_pause_ms),
        );
        crate::phase_telemetry::add(
            "preparation_cache_measurement_pacer_load_wait",
            Duration::from_millis(load_pause_ms),
        );
        crate::phase_telemetry::add(
            "preparation_cache_measurement_scan_limiter_sleep",
            Duration::from_millis(scan_sleep_ms),
        );
        let completed_at_ms = now_ms();
        match run {
            Ok(measured) => {
                let per_second = |count: u64| {
                    count
                        .saturating_mul(1_000)
                        .checked_div(duration_ms.max(1))
                        .unwrap_or(0)
                };
                let previous = self.shared_cache_measurement_result();
                let mut result = SharedCacheMeasurement {
                    schema: RESULT_SCHEMA.into(),
                    repository: request.repository.clone(),
                    cache_root_key: request.cache_root_key.clone(),
                    root_identity: measured.root_identity,
                    store_identity: measured.store_identity,
                    budget_bytes: request.budget_bytes,
                    outcome: measured.outcome,
                    complete: measured.complete,
                    bytes: measured.bytes,
                    entries: measured.entries,
                    skipped_entries: measured.skipped,
                    concurrent_activity: measured.concurrent_activity,
                    over_budget_walks: 0,
                    over_budget_confirmed: false,
                    started_at_ms: running_since_ms,
                    completed_at_ms,
                    duration_ms,
                    entries_per_second: per_second(measured.entries),
                    bytes_per_second: per_second(measured.bytes),
                    pacer_memory_pause_ms: memory_pause_ms,
                    pacer_load_pause_ms: load_pause_ms,
                    pacer_scan_limiter_sleep_ms: scan_sleep_ms,
                    attempt,
                    trusted_for_ms: MIN_TRUST_MS,
                    trust_reason: String::new(),
                    used_at_ms: None,
                    times_used: 0,
                };
                if result.outcome == MeasurementOutcome::OverBudget {
                    let earlier = previous
                        .as_ref()
                        .filter(|previous| {
                            previous.outcome == MeasurementOutcome::OverBudget
                                && previous.same_store(&result)
                        })
                        .map_or(0, |previous| previous.over_budget_walks);
                    result.over_budget_walks = earlier.saturating_add(1);
                    result.over_budget_confirmed =
                        result.over_budget_walks >= OVER_BUDGET_CONFIRMATIONS;
                }
                let (trusted_for_ms, trust_reason) = trust_for(previous.as_ref(), &result);
                result.trusted_for_ms = trusted_for_ms;
                result.trust_reason = trust_reason.to_string();
                let value = serde_json::to_string(&result)?;
                self.store().meta_set(RESULT_KEY, &value)?;
                let awaiting_confirmation = result.outcome == MeasurementOutcome::OverBudget
                    && !result.over_budget_confirmed;
                // Clear the request (a coalesce during the walk is answered
                // by it), or turn it into the confirming walk.
                self.finish_measurement_request(attempt, running_since_ms, |current| {
                    awaiting_confirmation.then(|| SharedCacheMeasurementRequest {
                        reason: "confirm_over_budget".into(),
                        queued_at_ms: completed_at_ms,
                        times_queued: 1,
                        attempts: 0,
                        retry_after_ms: Some(completed_at_ms.saturating_add(CONFIRMATION_DELAY_MS)),
                        last_interruption: None,
                        consecutive_failures: 0,
                        failure_store_identity: None,
                        ..current
                    })
                });
                crate::phase_telemetry::add(
                    match (result.outcome, result.over_budget_confirmed) {
                        (MeasurementOutcome::WithinBudget, _) => {
                            "preparation_cache_measurement_completed_within_budget"
                        }
                        (MeasurementOutcome::OverBudget, true) => {
                            "preparation_cache_measurement_completed_over_budget"
                        }
                        (MeasurementOutcome::OverBudget, false) => {
                            "preparation_cache_measurement_completed_over_budget_unconfirmed"
                        }
                        (MeasurementOutcome::WithinMargin, _) => {
                            "preparation_cache_measurement_completed_within_margin"
                        }
                    },
                    Duration::ZERO,
                );
                if result.concurrent_activity {
                    crate::phase_telemetry::add(
                        "preparation_cache_measurement_concurrent_activity",
                        Duration::ZERO,
                    );
                }
                let summary = format!(
                    "{} ({} bytes, {} entries{}{})",
                    result.outcome.as_str(),
                    result.bytes,
                    result.entries,
                    if result.complete {
                        ""
                    } else {
                        ", stopped once over the margin"
                    },
                    if result.concurrent_activity {
                        ", concurrent activity"
                    } else {
                        ""
                    }
                );
                let settles = result.outcome == MeasurementOutcome::WithinBudget
                    || result.over_budget_confirmed;
                if result.over_budget_confirmed
                    && let Some(binding) = binding
                {
                    self.make_shared_cache_budget_check_due(&binding.path, completed_at_ms);
                }
                self.note_background_measurement_on_inconclusive_record(
                    settles.then_some(&result),
                    if settles {
                        format!("settled by a background measurement: {summary}")
                    } else if awaiting_confirmation {
                        format!(
                            "a background measurement found {summary}; a confirming walk is queued before any rotation"
                        )
                    } else {
                        format!(
                            "a background measurement found {summary}: over the budget but within the {BACKGROUND_MARGIN_PERCENT}% margin, so the store is kept"
                        )
                    },
                );
                Ok(MeasurementLaneOutcome::Completed(result))
            }
            Err(interrupted) => {
                crate::phase_telemetry::add(interrupted.reason.counter(), Duration::ZERO);
                let interruption = SharedCacheMeasurementInterruption {
                    reason: interrupted.reason,
                    detail: interrupted.detail.clone(),
                    at_ms: completed_at_ms,
                    attempt,
                    entries: interrupted.entries,
                    bytes: interrupted.bytes,
                    duration_ms,
                    pacer_memory_pause_ms: memory_pause_ms,
                    pacer_load_pause_ms: load_pause_ms,
                };
                self.reset_over_budget_confirmation();
                let reason = interrupted.reason;
                let store_identity = interrupted.store_identity.clone();
                let mut retry_after_ms = None;
                self.finish_measurement_request(attempt, running_since_ms, |mut current| {
                    if reason.drops_request() {
                        return None;
                    }
                    let mut delay = RETRY_AFTER_INTERRUPTION_MS;
                    if reason.backs_off() {
                        // Reset when the store changed since the last failure.
                        current.consecutive_failures =
                            if current.failure_store_identity == store_identity {
                                current.consecutive_failures.saturating_add(1)
                            } else {
                                1
                            };
                        current.failure_store_identity = store_identity.clone();
                        delay = retry_delay_ms(current.consecutive_failures);
                    }
                    current.retry_after_ms = Some(completed_at_ms.saturating_add(delay));
                    current.last_interruption = Some(interruption.clone());
                    Some(current)
                });
                if !reason.drops_request() {
                    retry_after_ms = self
                        .shared_cache_measurement_request()
                        .and_then(|request| request.retry_after_ms);
                }
                self.note_background_measurement_on_inconclusive_record(
                    None,
                    format!(
                        "background measurement attempt {attempt} interrupted ({}) after {} entries and {} bytes{}",
                        interruption.reason.as_str(),
                        interruption.entries,
                        interruption.bytes,
                        match retry_after_ms {
                            _ if reason.drops_request() => {
                                "; the next inconclusive check queues the current root".to_string()
                            }
                            Some(after) => format!("; it restarts after {after} (ms since epoch)"),
                            None => String::new(),
                        }
                    ),
                );
                Ok(MeasurementLaneOutcome::Interrupted(interruption))
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn run_preparation_cache_measurement_lane(
        &mut self,
        _host_lock_directory: Option<&Path>,
    ) -> Result<MeasurementLaneOutcome, BrokerOpError> {
        Ok(MeasurementLaneOutcome::NotDue)
    }

    /// Make the next budget check due (not overdue) so an over-budget store
    /// rotates at the next prepare that finds the cache free.
    fn make_shared_cache_budget_check_due(&mut self, cache_root: &Path, now: i64) {
        let key = super::shared_cache_budget_checked_key(cache_root);
        let due_at = now.saturating_sub(SHARED_CACHE_BUDGET_CHECK_INTERVAL_MS);
        let current = self
            .store_ref()
            .meta_get(&key)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok());
        if current.is_some_and(|checked| checked <= due_at) {
            return;
        }
        let written = self
            .store()
            .meta_set(&key, &due_at.to_string())
            .map_err(|error| error.to_string());
        crate::warn_unrecorded("make the shared-cache budget check due", written);
    }
}

/// Back-off after the `failures`-th consecutive deadline or refused walk of
/// one store: 10 minutes, doubling, at most a day.
pub(crate) fn retry_delay_ms(failures: u32) -> i64 {
    let doublings = failures.saturating_sub(1).min(16);
    RETRY_AFTER_INTERRUPTION_MS
        .saturating_mul(1_i64 << doublings)
        .min(MAX_RETRY_AFTER_MS)
}

enum HostMeasurementSlot {
    Held(crate::file_lock::ExclusiveFileLock),
    /// No host state directory could hold the lock. The repository worker
    /// lock is already released when the walk would start, so nothing would
    /// serialise it: the lane skips the measurement instead.
    Unavailable,
    Busy,
}

fn acquire_host_measurement_slot(directory: Option<&Path>) -> HostMeasurementSlot {
    let Some(directory) = directory else {
        return HostMeasurementSlot::Unavailable;
    };
    if std::fs::create_dir_all(directory).is_err() {
        return HostMeasurementSlot::Unavailable;
    }
    match crate::file_lock::open_lock_file(&directory.join(HOST_LOCK_FILE))
        .and_then(crate::file_lock::ExclusiveFileLock::try_acquire)
    {
        Ok(Some(lock)) => HostMeasurementSlot::Held(lock),
        Ok(None) => HostMeasurementSlot::Busy,
        Err(_) => HostMeasurementSlot::Unavailable,
    }
}

#[derive(Debug)]
struct Interrupted {
    reason: MeasurementInterruptionReason,
    detail: Option<String>,
    entries: u64,
    bytes: u64,
    /// The store's identity when the walk began, for the failure back-off.
    store_identity: Option<String>,
}

impl Interrupted {
    fn new(reason: MeasurementInterruptionReason, detail: Option<String>) -> Self {
        Self {
            reason,
            detail,
            entries: 0,
            bytes: 0,
            store_identity: None,
        }
    }
}

#[derive(Debug)]
struct Measured {
    root_identity: Option<String>,
    store_identity: Option<String>,
    outcome: MeasurementOutcome,
    complete: bool,
    bytes: u64,
    entries: u64,
    skipped: u64,
    concurrent_activity: bool,
}

struct MeasureRequest<'a> {
    cache_root: &'a Path,
    binding_identity: Option<&'a [u8]>,
    expected_root_identity: Option<&'a str>,
    repository: &'a str,
    budget: u64,
    resource_db_path: Option<&'a Path>,
    max_elapsed: Duration,
    /// Called before each directory open; tests use it to change the tree.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    before_open: &'a mut dyn FnMut(super::SharedCacheScanOpenTarget),
}

/// Whether a `prepare` of `repository` reports holding the shared cache now:
/// one small directory read beside the host resource database.
fn prepare_holds_cache(resource_db_path: Option<&Path>, repository: &str) -> bool {
    resource_db_path.is_some_and(|path| {
        super::cache_activity::read_current(path, Some(repository), None, now_ms())
            .iter()
            .any(|record| record.operation.holds_cache())
    })
}

/// Walk one repository's store without a lease. See the module comment for
/// why the result is consistent enough to decide whether a rotation is due.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn measure_store(request: MeasureRequest<'_>) -> Result<Measured, Interrupted> {
    use std::cell::Cell;

    use super::{
        SHARED_CACHE_SCAN_MAX_DEPTH, SharedCacheInventoryLimit, SharedCacheScanConcurrency,
        SharedCacheScanLimits, directory_inventory_fd_relative_walk,
    };

    let MeasureRequest {
        cache_root,
        binding_identity,
        expected_root_identity,
        repository,
        budget,
        resource_db_path,
        max_elapsed,
        before_open,
    } = request;
    let Some(root_before) = identity_digest(cache_root) else {
        return Err(Interrupted::new(
            MeasurementInterruptionReason::RootUnavailable,
            Some("the cache root could not be identified".into()),
        ));
    };
    if expected_root_identity.is_some_and(|expected| expected != root_before) {
        return Err(Interrupted::new(
            MeasurementInterruptionReason::RootChanged,
            Some("the cache root's identity changed since the request was queued".into()),
        ));
    }
    if let Some(resource_db_path) = resource_db_path
        && super::probe_rotation_recovery(
            cache_root,
            binding_identity,
            resource_db_path,
            ROTATION_PROBE_MAX_ENTRIES,
            Instant::now() + ROTATION_PROBE_BUDGET,
        ) != rotation_debt::DebtProbe::NoDebt
    {
        return Err(Interrupted::new(
            MeasurementInterruptionReason::RotationPending,
            None,
        ));
    }
    let store = cache_root.join(repository);
    let store_before = identity_digest(&store);
    let held_at_start = prepare_holds_cache(resource_db_path, repository);
    let threshold = over_budget_threshold(budget);
    let deadline = Instant::now() + max_elapsed;
    let seen = Cell::new((0_u64, 0_u64));
    let memory_stop = Cell::new(false);
    let mut on_entry = |entries: u64, bytes: u64| -> Result<(), PreparationError> {
        seen.set((entries, bytes));
        // At the first entry and then once per batch.
        if (entries.saturating_sub(1)).is_multiple_of(PACING_BATCH_ENTRIES)
            && unattended_pacing::before_unattended_scan_batch(
                Some(deadline),
                if entries <= 1 {
                    0
                } else {
                    PACING_BATCH_ENTRIES
                },
                SCAN_ENTRIES_PER_SECOND,
            ) == unattended_pacing::PressureVerdict::MemoryBackoff
        {
            memory_stop.set(true);
            return Err(PreparationError::LivenessIncomplete {
                reason: "memory pressure".into(),
            });
        }
        Ok(())
    };
    let walked = directory_inventory_fd_relative_walk(
        &store,
        SharedCacheScanLimits {
            max_entries: usize::MAX,
            max_depth: SHARED_CACHE_SCAN_MAX_DEPTH,
            max_elapsed,
        },
        Some(threshold),
        SharedCacheScanConcurrency::Concurrent,
        before_open,
        &mut |_| Ok(()),
        &mut on_entry,
    );
    let (entries_seen, bytes_seen) = seen.get();
    let interrupted = |reason, detail| Interrupted {
        reason,
        detail,
        entries: entries_seen,
        bytes: bytes_seen,
        store_identity: store_before.clone(),
    };
    let (inventory, counts) = match walked {
        Ok(walked) => walked,
        Err(_) if memory_stop.get() => {
            return Err(interrupted(
                MeasurementInterruptionReason::MemoryPressure,
                None,
            ));
        }
        Err(error) => {
            let detail = match error {
                PreparationError::StorageUnavailable { reason, .. } => reason,
                _ => "the walk could not read the store".into(),
            };
            return Err(interrupted(
                MeasurementInterruptionReason::ScanRefused,
                Some(detail),
            ));
        }
    };
    // A rotation, remount or replacement while the walk ran means it may
    // have counted a tree that is no longer the store: discard it.
    if identity_digest(cache_root).as_deref() != Some(root_before.as_str())
        || identity_digest(&store) != store_before
    {
        return Err(interrupted(
            MeasurementInterruptionReason::RootChanged,
            Some("the cache root or the store changed while it was measured".into()),
        ));
    }
    let concurrent_activity =
        counts.skipped > 0 || held_at_start || prepare_holds_cache(resource_db_path, repository);
    let measured = |outcome, complete, bytes| Measured {
        root_identity: Some(root_before.clone()),
        store_identity: store_before.clone(),
        outcome,
        complete,
        bytes,
        entries: counts.entries,
        skipped: counts.skipped,
        concurrent_activity,
    };
    match inventory {
        SharedCacheInventory::Absent => Ok(measured(MeasurementOutcome::WithinBudget, true, 0)),
        SharedCacheInventory::Complete { bytes } if bytes <= budget => {
            Ok(measured(MeasurementOutcome::WithinBudget, true, bytes))
        }
        SharedCacheInventory::Complete { bytes } if bytes <= threshold => {
            Ok(measured(MeasurementOutcome::WithinMargin, true, bytes))
        }
        SharedCacheInventory::Complete { bytes } => {
            Ok(measured(MeasurementOutcome::OverBudget, true, bytes))
        }
        SharedCacheInventory::OverBudget {
            observed_bytes_lower_bound,
        } => Ok(measured(
            MeasurementOutcome::OverBudget,
            false,
            observed_bytes_lower_bound,
        )),
        SharedCacheInventory::BoundedIncomplete {
            limit: SharedCacheInventoryLimit::Time,
            ..
        } => Err(interrupted(MeasurementInterruptionReason::Deadline, None)),
        SharedCacheInventory::BoundedIncomplete { .. } => Err(interrupted(
            MeasurementInterruptionReason::ScanRefused,
            Some("the walk stopped at an entry limit".into()),
        )),
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::broker::PreparationCacheRootBinding;
    use crate::gc::unattended_pacing::{
        FakeClock, PressureSample, Scripted, UnattendedPacer, UnattendedPacingScope, UnlinkRate,
    };
    use crate::preparation::{
        SHARED_CACHE_SCAN_MAX_DEPTH, SHARED_CACHE_SCAN_MAX_ENTRIES, SharedCacheBudget,
        SharedCacheBudgetRequest, SharedCacheRotationReason, SharedCacheScanConcurrency,
        SharedCacheScanLimits, SharedCacheScanOpenTarget, cache_activity,
        directory_inventory_fd_relative_walk, shared_cache_budget_checked_key,
        shared_cache_budget_with_background,
    };

    // Rotation recovery validates the store name as a repository digest.
    const REPOSITORY: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    struct Fixture {
        repository: tempfile::TempDir,
        storage: crate::test_storage::TestStorage,
        broker: Broker,
        cache: tempfile::TempDir,
        cache_root: PathBuf,
        host: tempfile::TempDir,
    }

    impl Fixture {
        fn store(&self) -> PathBuf {
            self.cache_root.join(REPOSITORY)
        }

        fn bindings(&self) -> Vec<PreparationCacheRootBinding> {
            vec![PreparationCacheRootBinding {
                path: self.cache_root.clone(),
                identity: None,
                instance_nonce: None,
                root_guard: None,
            }]
        }

        fn db(&self) -> PathBuf {
            self.cache_root
                .with_file_name(".aethyme-test-host-resources.sqlite3")
        }

        /// A second broker handle on the same repository database, as
        /// another process (a prepare or a second worker) would have.
        fn other_broker(&self) -> Broker {
            self.storage.open(self.repository.path()).unwrap()
        }

        fn queue(&mut self, budget: u64) {
            let cache_root = self.cache_root.clone();
            let report = self
                .broker
                .follow_up_shared_cache_budget_check(
                    Some("inconclusive_time_limit"),
                    REPOSITORY,
                    &cache_root,
                    budget,
                    None,
                )
                .expect("an inconclusive check queues a measurement");
            assert_eq!(report.queued.as_deref(), Some("inconclusive_time_limit"));
        }

        fn run_lane(&mut self) -> MeasurementLaneOutcome {
            let bindings = self.bindings();
            let host = self.host.path().to_path_buf();
            self.broker
                .run_preparation_cache_measurement_lane_with(Some(&host), &bindings, None)
                .unwrap()
        }

        fn run_lane_hooked(
            &mut self,
            before_open: &mut dyn FnMut(SharedCacheScanOpenTarget),
        ) -> MeasurementLaneOutcome {
            let bindings = self.bindings();
            let host = self.host.path().to_path_buf();
            self.broker
                .run_preparation_cache_measurement_lane_hooked(
                    Some(&host),
                    &bindings,
                    None,
                    before_open,
                )
                .unwrap()
        }

        fn write_request(&mut self, request: &SharedCacheMeasurementRequest) {
            let value = serde_json::to_string(&Some(request)).unwrap();
            self.broker.store().meta_set(REQUEST_KEY, &value).unwrap();
        }

        fn write_result(&mut self, result: &SharedCacheMeasurement) {
            let value = serde_json::to_string(result).unwrap();
            self.broker.store().meta_set(RESULT_KEY, &value).unwrap();
        }

        /// Let the request's back-off elapse, as time passing would.
        fn expire_retry(&mut self) {
            let mut request = self.broker.shared_cache_measurement_request().unwrap();
            request.retry_after_ms = Some(0);
            self.write_request(&request);
        }

        fn raw_request(&self) -> Option<String> {
            self.broker.store_ref().meta_get(REQUEST_KEY).unwrap()
        }

        fn budget_request<'a>(
            &'a self,
            budget: &'a SharedCacheBudget,
            resource_db_path: &'a Path,
        ) -> SharedCacheBudgetRequest<'a> {
            SharedCacheBudgetRequest {
                budget: Some(budget),
                cache_root: &self.cache_root,
                resource_db_path,
                repository: REPOSITORY,
                digest: "0123456789ab",
                expected_root_identity: None,
                root_binding: None,
            }
        }

        fn fill(&self, files: usize, file_bytes: usize) {
            let nested = self.store().join("nested");
            fs::create_dir_all(&nested).unwrap();
            for index in 0..files {
                fs::write(nested.join(format!("file-{index}")), vec![0_u8; file_bytes]).unwrap();
            }
        }

        fn empty_store(&self) {
            fs::remove_dir_all(self.store().join("nested")).unwrap();
            fs::create_dir(self.store().join("nested")).unwrap();
        }
    }

    fn fixture(files: usize, file_bytes: usize) -> Fixture {
        let repository = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["commit", "--allow-empty", "-qm", "fixture"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(repository.path())
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(repository.path()).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let cache_root = cache
            .path()
            .canonicalize()
            .unwrap()
            .join("preparation-cache");
        let fixture = Fixture {
            repository,
            storage,
            broker,
            cache,
            cache_root,
            host: tempfile::tempdir().unwrap(),
        };
        fixture.fill(files, file_bytes);
        fixture
    }

    fn pacer(samples: Vec<PressureSample>) -> UnattendedPacingScope {
        unattended_pacing::install(UnattendedPacer::new(
            UnlinkRate::Limited(1_000),
            Box::new(FakeClock::new()),
            Box::new(Scripted {
                samples,
                reads: Default::default(),
            }),
        ))
    }

    fn calm() -> PressureSample {
        PressureSample {
            memory_elevated: Some(false),
            load_elevated: Some(false),
        }
    }

    fn memory_pressure() -> PressureSample {
        PressureSample {
            memory_elevated: Some(true),
            load_elevated: Some(false),
        }
    }

    /// A foreground inventory with no time at all is always inconclusive, so
    /// any other outcome proves the decision came from the measurement.
    fn no_time() -> SharedCacheScanLimits {
        SharedCacheScanLimits {
            max_entries: SHARED_CACHE_SCAN_MAX_ENTRIES,
            max_depth: SHARED_CACHE_SCAN_MAX_DEPTH,
            max_elapsed: Duration::ZERO,
        }
    }

    fn inventory_summary(
        guard: cache_activity::ActivityGuard,
    ) -> cache_activity::PreparationCacheOperationSummary {
        guard
            .finish()
            .into_iter()
            .find(|operation| {
                operation.operation == cache_activity::PreparationCacheOperation::BudgetInventory
            })
            .expect("the budget check is summarized")
    }

    fn activity() -> cache_activity::ActivityGuard {
        cache_activity::install_with(
            None,
            "repository",
            1,
            Duration::from_secs(3600),
            Box::new(|_: &str| {}),
        )
    }

    fn completed(outcome: MeasurementLaneOutcome) -> SharedCacheMeasurement {
        match outcome {
            MeasurementLaneOutcome::Completed(result) => result,
            other => panic!("expected a completed measurement, got {other:?}"),
        }
    }

    fn interrupted(outcome: MeasurementLaneOutcome) -> SharedCacheMeasurementInterruption {
        match outcome {
            MeasurementLaneOutcome::Interrupted(interruption) => interruption,
            other => panic!("expected an interrupted measurement, got {other:?}"),
        }
    }

    fn make_fifo(path: &Path) {
        let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: the path is NUL-terminated and outlives the call; the mode
        // is a plain value.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }

    fn budget(max_bytes: u64) -> SharedCacheBudget {
        SharedCacheBudget {
            max_bytes,
            ..SharedCacheBudget::default()
        }
    }

    #[test]
    fn an_inconclusive_check_queues_one_coalesced_background_measurement() {
        let mut fixture = fixture(1, 10);
        assert!(!fixture.broker.shared_cache_measurement_due(now_ms()));
        fixture.queue(1_000);
        let request = fixture.broker.shared_cache_measurement_request().unwrap();
        assert_eq!(request.schema, REQUEST_SCHEMA);
        assert_eq!(request.repository, REPOSITORY);
        assert_eq!(request.cache_root_key, cache_root_key(&fixture.cache_root));
        assert_eq!(request.root_identity, identity_digest(&fixture.cache_root));
        assert!(request.root_identity.is_some());
        assert_eq!((request.times_queued, request.attempts), (1, 0));
        assert!(fixture.broker.shared_cache_measurement_due(now_ms()));

        let cache_root = fixture.cache_root.clone();
        fixture.broker.follow_up_shared_cache_budget_check(
            Some("inconclusive_entry_limit"),
            REPOSITORY,
            &cache_root,
            2_000,
            None,
        );
        let request = fixture.broker.shared_cache_measurement_request().unwrap();
        assert_eq!(request.times_queued, 2);
        assert_eq!(request.reason, "inconclusive_entry_limit");
        assert_eq!(request.budget_bytes, 2_000);

        assert!(
            fixture
                .broker
                .follow_up_shared_cache_budget_check(
                    Some("within_budget"),
                    REPOSITORY,
                    &cache_root,
                    2_000,
                    None,
                )
                .is_none()
        );
        let status = fixture.broker.shared_cache_measurement_status().unwrap();
        assert!(status.request.is_some() && status.result.is_none());
        assert!(!status.result_fresh && !status.running);
    }

    #[test]
    fn a_within_budget_measurement_settles_the_next_check_without_an_inventory() {
        let mut fixture = fixture(10, 100);
        fixture.queue(10_000);
        let _pacing = pacer(vec![calm()]);
        let result = completed(fixture.run_lane());
        assert_eq!(result.outcome, MeasurementOutcome::WithinBudget);
        assert!(result.complete);
        assert_eq!(result.bytes, 1_000);
        assert_eq!(result.entries, 11, "ten files and their directory");
        assert_eq!((result.skipped_entries, result.attempt), (0, 1));
        assert!(!result.concurrent_activity);
        assert_eq!(result.store_identity, identity_digest(&fixture.store()));
        // 10% of the budget and no growth rate yet: trusted for a day.
        assert_eq!(result.trusted_for_ms, MAX_TRUST_MS);
        assert_eq!(result.trust_reason, "at_most_half_of_budget");
        assert!(fixture.broker.shared_cache_measurement_request().is_none());
        let fresh = fixture
            .broker
            .fresh_shared_cache_measurement(REPOSITORY, &fixture.cache_root, 10_000, now_ms())
            .expect("a fresh result bound to the root is used");
        assert_eq!(fresh, result);

        let budget = budget(10_000);
        let db = fixture.db();
        let guard = activity();
        let rotation = shared_cache_budget_with_background(
            fixture.budget_request(&budget, &db),
            Some(&fresh),
            &mut |_| Ok(()),
            no_time(),
        )
        .unwrap();
        assert_eq!(rotation, None);
        let summary = inventory_summary(guard);
        assert_eq!(summary.outcome, Some("within_budget_background"));
        assert_eq!(summary.items_done, 11);
        assert_eq!(summary.bytes_observed, Some(1_000));

        // Young in its trust: decided from it, no refresh walk.
        let cache_root = fixture.cache_root.clone();
        let report = fixture
            .broker
            .follow_up_shared_cache_budget_check(
                summary.outcome,
                REPOSITORY,
                &cache_root,
                10_000,
                Some(&fresh),
            )
            .unwrap();
        assert_eq!(
            report.decided_by.expect("the source is reported").bytes,
            1_000
        );
        assert_eq!(report.queued, None, "no hourly refresh of a healthy store");
        let stored = fixture.broker.shared_cache_measurement_result().unwrap();
        assert_eq!(stored.times_used, 1);

        // In the last quarter of its trust it queues a refresh.
        let mut late = fresh.clone();
        late.completed_at_ms = now_ms() - late.trusted_for_ms * 3 / 4 - 1_000;
        let report = fixture
            .broker
            .follow_up_shared_cache_budget_check(
                Some("within_budget_background"),
                REPOSITORY,
                &cache_root,
                10_000,
                Some(&late),
            )
            .unwrap();
        assert_eq!(report.queued.as_deref(), Some("refresh"));
    }

    #[test]
    fn trust_scales_with_headroom_and_the_observed_growth_rate() {
        let base = SharedCacheMeasurement {
            schema: RESULT_SCHEMA.into(),
            repository: REPOSITORY.into(),
            cache_root_key: "key".into(),
            root_identity: Some("root".into()),
            store_identity: Some("store".into()),
            budget_bytes: 1_000,
            outcome: MeasurementOutcome::WithinBudget,
            complete: true,
            bytes: 400,
            entries: 1,
            skipped_entries: 0,
            concurrent_activity: false,
            over_budget_walks: 0,
            over_budget_confirmed: false,
            started_at_ms: 0,
            completed_at_ms: 10 * MIN_TRUST_MS,
            duration_ms: 1,
            entries_per_second: 0,
            bytes_per_second: 0,
            pacer_memory_pause_ms: 0,
            pacer_load_pause_ms: 0,
            pacer_scan_limiter_sleep_ms: 0,
            attempt: 1,
            trusted_for_ms: MIN_TRUST_MS,
            trust_reason: String::new(),
            used_at_ms: None,
            times_used: 0,
        };
        let at = |bytes| SharedCacheMeasurement {
            bytes,
            ..base.clone()
        };
        assert_eq!(trust_for(None, &at(400)).0, MAX_TRUST_MS);
        assert_eq!(trust_for(None, &at(700)).0, 6 * 60 * 60 * 1000);
        assert_eq!(trust_for(None, &at(900)).0, MIN_TRUST_MS);
        // 100 bytes in 2 h with 500 left: 10 h until the budget.
        let previous = SharedCacheMeasurement {
            completed_at_ms: base.completed_at_ms - 2 * MIN_TRUST_MS,
            bytes: 400,
            ..base.clone()
        };
        let (trust, reason) = trust_for(Some(&previous), &at(500));
        assert_eq!((trust, reason), (10 * MIN_TRUST_MS, "growth_rate"));
        // Fast growth is clamped to the budget-check interval.
        assert_eq!(trust_for(Some(&previous), &at(990)).0, MIN_TRUST_MS);
        assert_eq!(trust_for(Some(&previous), &at(400)).1, "not_growing");
        // Another store's result is not a growth sample.
        let other = SharedCacheMeasurement {
            store_identity: Some("other".into()),
            ..previous
        };
        assert_eq!(
            trust_for(Some(&other), &at(500)).1,
            "at_most_half_of_budget"
        );
    }

    #[test]
    fn one_over_budget_walk_does_not_rotate_two_do_and_a_within_walk_resets() {
        let mut fixture = fixture(20, 100);
        let cache_root = fixture.cache_root.clone();
        let stamp = shared_cache_budget_checked_key(&cache_root);
        let checked_at = now_ms();
        fixture
            .broker
            .store()
            .meta_set(&stamp, &checked_at.to_string())
            .unwrap();
        fixture.queue(150);
        let _pacing = pacer(vec![calm()]);

        // First over-budget walk: recorded, unconfirmed, confirmation queued.
        let first = completed(fixture.run_lane());
        assert_eq!(first.outcome, MeasurementOutcome::OverBudget);
        assert!(!first.complete, "the walk stops once over the margin");
        assert_eq!(first.bytes, 200, "it stopped at the second file");
        assert_eq!(
            (first.over_budget_walks, first.over_budget_confirmed),
            (1, false)
        );
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement(REPOSITORY, &cache_root, 150, now_ms())
                .is_none(),
            "an unconfirmed over-budget result never rotates"
        );
        let confirm = fixture.broker.shared_cache_measurement_request().unwrap();
        assert_eq!(confirm.reason, "confirm_over_budget");
        assert!(confirm.retry_after_ms.is_some_and(|after| after > now_ms()));
        assert_eq!(
            fixture.broker.store().meta_get(&stamp).unwrap(),
            Some(checked_at.to_string())
        );

        // The store shrinks before the confirming walk: within budget resets.
        fixture.empty_store();
        fixture.expire_retry();
        let within = completed(fixture.run_lane());
        assert_eq!(within.outcome, MeasurementOutcome::WithinBudget);
        assert_eq!(within.over_budget_walks, 0);
        assert!(fixture.broker.shared_cache_measurement_request().is_none());

        // It grows again: two consecutive over-budget walks confirm.
        fixture.fill(20, 100);
        fixture.queue(150);
        assert!(!completed(fixture.run_lane()).over_budget_confirmed);
        fixture.expire_retry();
        let confirmed = completed(fixture.run_lane());
        assert_eq!(
            (confirmed.over_budget_walks, confirmed.over_budget_confirmed),
            (2, true)
        );
        assert!(fixture.broker.shared_cache_measurement_request().is_none());
        let stamped: i64 = fixture
            .broker
            .store()
            .meta_get(&stamp)
            .unwrap()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            crate::preparation::budget_check_need(Some(stamped), now_ms()),
            Some(crate::preparation::ExclusiveNeed::BudgetCheckDue)
        );
        let fresh = fixture
            .broker
            .fresh_shared_cache_measurement(REPOSITORY, &cache_root, 150, now_ms())
            .expect("a confirmed result decides");

        let budget = budget(150);
        let db = fixture.db();
        let guard = activity();
        let rotation = shared_cache_budget_with_background(
            fixture.budget_request(&budget, &db),
            Some(&fresh),
            &mut |_| Ok(()),
            no_time(),
        )
        .unwrap()
        .expect("a confirmed over-budget measurement rotates the store");
        assert!(rotation.removed);
        assert_eq!(
            rotation.rotation_reason,
            SharedCacheRotationReason::BudgetExceeded
        );
        assert_eq!(rotation.observed_bytes_lower_bound, 200);
        assert_eq!(fs::read_dir(fixture.store()).unwrap().count(), 0);
        assert_eq!(
            inventory_summary(guard).outcome,
            Some("over_budget_background")
        );
        fixture.broker.forget_shared_cache_measurement();
        assert!(fixture.broker.shared_cache_measurement_status().is_none());
    }

    #[test]
    fn an_interrupted_walk_between_two_over_budget_walks_resets_the_count() {
        let mut fixture = fixture(20, 100);
        fixture.queue(150);
        {
            let _pacing = pacer(vec![calm()]);
            completed(fixture.run_lane());
        }
        fixture.expire_retry();
        {
            let _pacing = pacer(vec![memory_pressure()]);
            interrupted(fixture.run_lane());
        }
        assert_eq!(
            fixture
                .broker
                .shared_cache_measurement_result()
                .unwrap()
                .over_budget_walks,
            0
        );
        fixture.expire_retry();
        let _pacing = pacer(vec![calm()]);
        let again = completed(fixture.run_lane());
        assert_eq!(
            (again.over_budget_walks, again.over_budget_confirmed),
            (1, false)
        );
    }

    #[test]
    fn a_result_between_the_budget_and_the_margin_neither_rotates_nor_settles() {
        // 1,020 bytes against 1,000: over the budget, under the 5% margin.
        let mut fixture = fixture(10, 102);
        assert_eq!(over_budget_threshold(1_000), 1_050);
        fixture.queue(1_000);
        let _pacing = pacer(vec![calm()]);
        let result = completed(fixture.run_lane());
        assert_eq!(result.outcome, MeasurementOutcome::WithinMargin);
        assert!(result.complete);
        assert_eq!(result.bytes, 1_020);
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement(REPOSITORY, &fixture.cache_root, 1_000, now_ms())
                .is_none()
        );
        let budget = budget(1_000);
        let db = fixture.db();
        let guard = activity();
        assert_eq!(
            shared_cache_budget_with_background(
                fixture.budget_request(&budget, &db),
                Some(&result),
                &mut |_| Ok(()),
                no_time(),
            )
            .unwrap(),
            None
        );
        assert_eq!(
            inventory_summary(guard).outcome,
            Some("inconclusive_time_limit"),
            "the check stays inconclusive and the store is kept"
        );
    }

    #[test]
    fn transient_bytes_seen_by_one_walk_never_rotate() {
        // A running step unpacks into the store while it is walked: the walk
        // counts the transient bytes and calls the store over budget.
        let mut fixture = fixture(1, 100);
        fixture.queue(1_000);
        let _pacing = pacer(vec![calm()]);
        let transient = fixture.store().join("nested").join("unpacking");
        let mut written = false;
        let result = completed(fixture.run_lane_hooked(&mut |target| {
            if matches!(target, SharedCacheScanOpenTarget::Child) && !written {
                written = true;
                fs::write(&transient, vec![0_u8; 4_000]).unwrap();
            }
        }));
        assert_eq!(result.outcome, MeasurementOutcome::OverBudget);
        assert!(!result.over_budget_confirmed);
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement(REPOSITORY, &fixture.cache_root, 1_000, now_ms())
                .is_none()
        );
        // The unpack finishes and moves its bytes elsewhere: the confirming
        // walk sees the real store, within budget, and nothing rotates.
        fs::remove_file(&transient).unwrap();
        fixture.expire_retry();
        let confirming = completed(fixture.run_lane());
        assert_eq!(confirming.outcome, MeasurementOutcome::WithinBudget);
        assert_eq!(confirming.over_budget_walks, 0);
    }

    #[test]
    fn a_result_for_a_changed_store_or_root_is_ignored() {
        let mut fixture = fixture(4, 100);
        fixture.queue(10_000);
        let _pacing = pacer(vec![calm()]);
        let result = completed(fixture.run_lane());

        let store = fixture.store();
        fs::rename(&store, fixture.cache_root.join("retired")).unwrap();
        fs::create_dir(&store).unwrap();
        let budget = budget(10_000);
        let db = fixture.db();
        let guard = activity();
        let rotation = shared_cache_budget_with_background(
            fixture.budget_request(&budget, &db),
            Some(&result),
            &mut |_| Ok(()),
            no_time(),
        )
        .unwrap();
        assert_eq!(rotation, None);
        assert_eq!(
            inventory_summary(guard).outcome,
            Some("inconclusive_time_limit"),
            "the foreground inventory ran instead"
        );

        let now = now_ms();
        let other_root = fixture.cache.path().join("elsewhere");
        fs::create_dir_all(&other_root).unwrap();
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement(REPOSITORY, &other_root, 10_000, now)
                .is_none()
        );
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement("another", &fixture.cache_root, 10_000, now)
                .is_none()
        );
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement(REPOSITORY, &fixture.cache_root, 100, now)
                .is_none(),
            "a within-budget result does not decide a smaller budget it exceeds"
        );
        let mut remounted = result.clone();
        remounted.root_identity = Some("0".repeat(32));
        fixture.write_result(&remounted);
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement(REPOSITORY, &fixture.cache_root, 10_000, now)
                .is_none()
        );
        let mut aged = result.clone();
        aged.completed_at_ms = now - aged.trusted_for_ms;
        fixture.write_result(&aged);
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement(REPOSITORY, &fixture.cache_root, 10_000, now)
                .is_none()
        );
        let mut partial = result.clone();
        partial.complete = false;
        partial.bytes = 300;
        fixture.write_result(&partial);
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement(REPOSITORY, &fixture.cache_root, 10_000, now)
                .is_none(),
            "a lower bound under budget decides nothing"
        );
    }

    #[test]
    fn a_rotation_during_the_walk_discards_it() {
        let mut fixture = fixture(3, 100);
        fixture.queue(10_000);
        let _pacing = pacer(vec![calm()]);
        let store = fixture.store();
        let retired = fixture.cache_root.join("retired");
        let mut rotated = false;
        let interruption = interrupted(fixture.run_lane_hooked(&mut |target| {
            if matches!(target, SharedCacheScanOpenTarget::Child) && !rotated {
                rotated = true;
                fs::rename(&store, &retired).unwrap();
                fs::create_dir(&store).unwrap();
            }
        }));
        assert_eq!(
            interruption.reason,
            MeasurementInterruptionReason::RootChanged
        );
        assert!(fixture.broker.shared_cache_measurement_result().is_none());
        assert!(
            fixture.broker.shared_cache_measurement_request().is_none(),
            "a changed root drops its request"
        );
    }

    #[test]
    fn a_pending_rotation_in_the_real_journal_stops_the_walk() {
        let mut fixture = fixture(3, 100);
        let db = fixture.db();
        let root = rotation_debt::DirectoryIdentity::capture(&fixture.cache_root).unwrap();
        let store = rotation_debt::DirectoryIdentity::capture(&fixture.store()).unwrap();
        let debt = rotation_debt::RotationDebt::new(
            REPOSITORY,
            &rotation_debt::new_transaction_id(),
            root.clone(),
            store,
        )
        .unwrap();
        let payload = rotation_debt::encode(&debt).unwrap();
        crate::preparation::rotation_journal::publish(
            &db,
            &fixture.cache_root,
            &root.encode().unwrap(),
            REPOSITORY,
            &payload,
        )
        .unwrap();
        assert_eq!(
            crate::preparation::probe_rotation_recovery(
                &fixture.cache_root,
                None,
                &db,
                4096,
                Instant::now() + Duration::from_secs(2),
            ),
            rotation_debt::DebtProbe::Pending
        );
        fixture.queue(10_000);
        let _pacing = pacer(vec![calm()]);
        let bindings = fixture.bindings();
        let host = fixture.host.path().to_path_buf();
        let outcome = fixture
            .broker
            .run_preparation_cache_measurement_lane_with(Some(&host), &bindings, Some(&db))
            .unwrap();
        assert_eq!(
            interrupted(outcome).reason,
            MeasurementInterruptionReason::RotationPending
        );
        assert!(fixture.broker.shared_cache_measurement_request().is_some());
    }

    #[test]
    fn memory_pressure_pauses_the_walk_and_a_persistent_one_interrupts_it() {
        let mut fixture = fixture(3, 100);
        fixture.queue(10_000);
        {
            let _pacing = pacer(vec![memory_pressure(), memory_pressure(), calm()]);
            let result = completed(fixture.run_lane());
            assert!(
                result.pacer_memory_pause_ms >= 2_000,
                "{} ms paused",
                result.pacer_memory_pause_ms
            );
        }

        fixture.queue(10_000);
        let _pacing = pacer(vec![memory_pressure()]);
        let interruption = interrupted(fixture.run_lane());
        assert_eq!(
            interruption.reason,
            MeasurementInterruptionReason::MemoryPressure
        );
        let request = fixture
            .broker
            .shared_cache_measurement_request()
            .expect("the request stays durable");
        assert_eq!(request.attempts, 1);
        assert_eq!(request.running_since_ms, None);
        assert!(request.retry_after_ms.is_some_and(|after| after > now_ms()));
        assert_eq!(request.last_interruption, Some(interruption));
    }

    #[test]
    fn the_scan_rate_limit_paces_the_walk() {
        let mut fixture = fixture(PACING_BATCH_ENTRIES as usize + 10, 1);
        fixture.queue(u64::MAX / 2);
        let _pacing = pacer(vec![calm()]);
        let result = completed(fixture.run_lane());
        // One full batch at 10,000 entries a second: about 410 ms slept on
        // the pacer's (fake) clock.
        assert!(
            result.pacer_scan_limiter_sleep_ms >= 400,
            "{} ms",
            result.pacer_scan_limiter_sleep_ms
        );
    }

    #[test]
    fn an_interrupted_measurement_restarts_after_its_back_off() {
        let mut fixture = fixture(3, 100);
        fixture.queue(10_000);
        {
            let _pacing = pacer(vec![memory_pressure()]);
            interrupted(fixture.run_lane());
        }
        let _pacing = pacer(vec![calm()]);
        assert_eq!(fixture.run_lane(), MeasurementLaneOutcome::NotDue);
        assert!(!fixture.broker.shared_cache_measurement_due(now_ms()));
        fixture.expire_retry();
        let result = completed(fixture.run_lane());
        assert_eq!(result.attempt, 2, "restarted from scratch");
        assert_eq!(result.bytes, 300);
        assert!(fixture.broker.shared_cache_measurement_request().is_none());
    }

    #[test]
    fn repeated_refused_walks_back_off_exponentially_until_the_store_changes() {
        assert_eq!(retry_delay_ms(1), RETRY_AFTER_INTERRUPTION_MS);
        assert_eq!(retry_delay_ms(2), 2 * RETRY_AFTER_INTERRUPTION_MS);
        assert_eq!(retry_delay_ms(3), 4 * RETRY_AFTER_INTERRUPTION_MS);
        assert_eq!(retry_delay_ms(40), MAX_RETRY_AFTER_MS);

        let mut fixture = fixture(1, 100);
        // A special file makes every walk of this store refuse.
        make_fifo(&fixture.store().join("nested").join("fifo"));
        fixture.queue(10_000);
        let _pacing = pacer(vec![calm()]);
        let mut delays = Vec::new();
        for _ in 0..3 {
            let before = now_ms();
            let interruption = interrupted(fixture.run_lane());
            assert_eq!(
                interruption.reason,
                MeasurementInterruptionReason::ScanRefused
            );
            let request = fixture.broker.shared_cache_measurement_request().unwrap();
            delays.push((
                request.consecutive_failures,
                request.retry_after_ms.unwrap() - before,
            ));
            fixture.expire_retry();
        }
        assert_eq!(
            delays.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(delays[2].1 >= 4 * RETRY_AFTER_INTERRUPTION_MS);

        // A new store (another identity) starts the back-off again.
        let store = fixture.store();
        fs::rename(&store, fixture.cache_root.join("retired")).unwrap();
        fs::create_dir_all(store.join("nested")).unwrap();
        make_fifo(&store.join("nested").join("fifo"));
        interrupted(fixture.run_lane());
        assert_eq!(
            fixture
                .broker
                .shared_cache_measurement_request()
                .unwrap()
                .consecutive_failures,
            1
        );
    }

    #[test]
    fn a_running_walk_is_never_restarted_or_overwritten() {
        let mut fixture = fixture(3, 100);
        fixture.queue(10_000);
        let mut request = fixture.broker.shared_cache_measurement_request().unwrap();
        request.attempts = 1;
        request.running_since_ms = Some(now_ms());
        request.running_pid = Some(std::process::id());
        fixture.write_request(&request);
        let before = fixture.raw_request();
        // Worker admission on broker open reads exactly this.
        assert!(!fixture.broker.shared_cache_measurement_due(now_ms()));
        let _pacing = pacer(vec![calm()]);
        assert_eq!(fixture.run_lane(), MeasurementLaneOutcome::AlreadyRunning);
        assert_eq!(fixture.raw_request(), before, "nothing was written");
        assert!(
            fixture
                .broker
                .shared_cache_measurement_status()
                .unwrap()
                .running
        );
    }

    #[test]
    fn a_walk_clears_its_request_despite_a_second_worker_and_a_coalesce() {
        let mut fixture = fixture(3, 100);
        fixture.queue(10_000);
        let mut other = fixture.other_broker();
        let bindings = fixture.bindings();
        let host = fixture.host.path().to_path_buf();
        let cache_root = fixture.cache_root.clone();
        let _pacing = pacer(vec![calm()]);
        let mut during = None;
        let result = completed(fixture.run_lane_hooked(&mut |target| {
            if matches!(target, SharedCacheScanOpenTarget::Child) && during.is_none() {
                // A broker open spawns a second worker: it must not walk or
                // write. A prepare's inconclusive check coalesces.
                let second = other
                    .run_preparation_cache_measurement_lane_with(Some(&host), &bindings, None)
                    .unwrap();
                other.queue_shared_cache_measurement(
                    REPOSITORY,
                    &cache_root,
                    10_000,
                    "inconclusive_time_limit",
                );
                during = Some(second);
            }
        }));
        assert_eq!(during, Some(MeasurementLaneOutcome::AlreadyRunning));
        assert_eq!(result.outcome, MeasurementOutcome::WithinBudget);
        assert!(
            fixture.broker.shared_cache_measurement_request().is_none(),
            "the completed walk answers the coalesced check too"
        );
    }

    #[test]
    fn an_interruption_keeps_its_back_off_despite_a_coalesce() {
        let mut fixture = fixture(3, 100);
        fixture.queue(10_000);
        let mut other = fixture.other_broker();
        let cache_root = fixture.cache_root.clone();
        let _pacing = pacer(vec![memory_pressure()]);
        let interruption = interrupted(fixture.run_lane_hooked(&mut |_| {
            other.queue_shared_cache_measurement(REPOSITORY, &cache_root, 10_000, "refresh");
        }));
        let request = fixture.broker.shared_cache_measurement_request().unwrap();
        assert!(request.retry_after_ms.is_some_and(|after| after > now_ms()));
        assert_eq!(request.last_interruption, Some(interruption));
        assert_eq!(request.running_since_ms, None);
    }

    #[test]
    fn a_stale_running_marker_is_taken_over() {
        for (since, pid) in [
            // Older than the walk deadline plus slack.
            (
                now_ms()
                    - i64::try_from(MAX_WALK_DURATION.as_millis()).unwrap()
                    - RUNNING_STALE_SLACK_MS
                    - 1,
                Some(std::process::id()),
            ),
            // Recent, but its process is gone.
            (now_ms(), Some(u32::MAX)),
        ] {
            let mut fixture = fixture(3, 100);
            fixture.queue(10_000);
            let mut request = fixture.broker.shared_cache_measurement_request().unwrap();
            request.attempts = 1;
            request.running_since_ms = Some(since);
            request.running_pid = pid;
            fixture.write_request(&request);
            assert!(fixture.broker.shared_cache_measurement_due(now_ms()));
            let _pacing = pacer(vec![calm()]);
            let result = completed(fixture.run_lane());
            assert_eq!(result.attempt, 2);
            assert!(fixture.broker.shared_cache_measurement_request().is_none());
        }
    }

    #[test]
    fn two_workers_never_measure_at_once() {
        let mut fixture = fixture(3, 100);
        fixture.queue(10_000);
        let _pacing = pacer(vec![calm()]);
        let held = acquire_host_measurement_slot(Some(fixture.host.path()));
        assert!(matches!(held, HostMeasurementSlot::Held(_)));
        assert!(matches!(
            acquire_host_measurement_slot(Some(fixture.host.path())),
            HostMeasurementSlot::Busy
        ));
        assert_eq!(fixture.run_lane(), MeasurementLaneOutcome::HostBusy);
        let request = fixture.broker.shared_cache_measurement_request().unwrap();
        assert_eq!(request.attempts, 0, "the loser did not walk");
        assert!(request.retry_after_ms.is_some_and(|after| after > now_ms()));
        assert!(fixture.broker.shared_cache_measurement_result().is_none());

        drop(held);
        fixture.expire_retry();
        completed(fixture.run_lane());
    }

    #[test]
    fn without_a_host_lock_directory_the_walk_is_skipped() {
        let mut fixture = fixture(3, 100);
        fixture.queue(10_000);
        let bindings = fixture.bindings();
        let outcome = fixture
            .broker
            .run_preparation_cache_measurement_lane_with(None, &bindings, None)
            .unwrap();
        assert_eq!(outcome, MeasurementLaneOutcome::HostLockUnavailable);
        let request = fixture.broker.shared_cache_measurement_request().unwrap();
        assert_eq!(request.attempts, 0);
        assert!(request.retry_after_ms.is_some());
    }

    #[test]
    fn the_lane_resolves_the_brokers_own_cache_root_binding() {
        let mut fixture = fixture(1, 10);
        let binding = fixture
            .broker
            .preparation_cache_root_bindings_for_maintenance()
            .unwrap()
            .into_iter()
            .next()
            .expect("the fixture broker has a preparation cache root");
        let store = binding.path.join(REPOSITORY);
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("payload"), vec![0_u8; 64]).unwrap();
        fixture.broker.queue_shared_cache_measurement(
            REPOSITORY,
            &binding.path,
            10_000,
            "inconclusive_time_limit",
        );
        let _pacing = pacer(vec![calm()]);
        let host = fixture.host.path().to_path_buf();
        let result = completed(
            fixture
                .broker
                .run_preparation_cache_measurement_lane(Some(&host))
                .unwrap(),
        );
        assert_eq!(result.bytes, 64);
        assert!(host.join(HOST_LOCK_FILE).exists());
    }

    #[test]
    fn a_settled_measurement_clears_the_notice_and_an_interrupted_one_explains_it() {
        let mut fixture = fixture(3, 100);
        let inventory = cache_activity::PreparationCacheOperationSummary {
            operation: cache_activity::PreparationCacheOperation::BudgetInventory,
            detail: None,
            elapsed_ms: 30_000,
            items_done: 420_000,
            bytes_observed: Some(3_000_000_000),
            outcome: Some("inconclusive_time_limit"),
        };
        for _ in 0..crate::preparation::SHARED_CACHE_INVENTORY_INCONCLUSIVE_ADVICE_AFTER {
            fixture
                .broker
                .record_shared_cache_inventory_outcome(&inventory);
        }
        assert!(
            fixture
                .broker
                .shared_cache_inventory_inconclusive()
                .is_some()
        );
        fixture.queue(10_000);
        {
            let _pacing = pacer(vec![memory_pressure()]);
            interrupted(fixture.run_lane());
        }
        let notice = fixture
            .broker
            .shared_cache_inventory_inconclusive()
            .expect("an unfinished measurement keeps the notice");
        assert!(
            notice
                .background_measurement
                .as_deref()
                .is_some_and(|note| note.contains("memory_pressure")),
            "{notice:?}"
        );
        fixture
            .broker
            .record_shared_cache_inventory_outcome(&inventory);
        assert!(
            fixture
                .broker
                .shared_cache_inventory_inconclusive()
                .and_then(|notice| notice.background_measurement)
                .is_some()
        );

        fixture.expire_retry();
        let _pacing = pacer(vec![calm()]);
        completed(fixture.run_lane());
        assert!(
            fixture
                .broker
                .shared_cache_inventory_inconclusive()
                .is_none(),
            "a settled measurement clears the notice"
        );
    }

    #[test]
    fn a_concurrent_walk_skips_and_counts_entries_that_vanish_and_an_exclusive_one_refuses() {
        let fixture = fixture(2, 100);
        let store = fixture.store();
        let walk = |concurrency| {
            let nested = store.join("nested");
            let mut removed = false;
            directory_inventory_fd_relative_walk(
                &store,
                SharedCacheScanLimits::default(),
                None,
                concurrency,
                |target| {
                    if matches!(target, SharedCacheScanOpenTarget::Child) && !removed {
                        removed = true;
                        fs::rename(&nested, store.join("..").join("moved-away")).unwrap();
                    }
                },
                &mut |_| Ok(()),
                &mut |_, _| Ok(()),
            )
        };
        let (inventory, counts) = walk(SharedCacheScanConcurrency::Concurrent).unwrap();
        assert_eq!(inventory, SharedCacheInventory::Complete { bytes: 0 });
        assert_eq!(counts.skipped, 1);
        fs::rename(fixture.cache_root.join("moved-away"), store.join("nested")).unwrap();
        assert!(walk(SharedCacheScanConcurrency::Exclusive).is_err());
    }
}
