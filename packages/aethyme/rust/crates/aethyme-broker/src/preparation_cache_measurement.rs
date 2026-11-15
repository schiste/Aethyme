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
//!   lanes, at background CPU and I/O priority, one at a time per host (a
//!   host-wide advisory lock beside the deleter lock), paced by the
//!   unattended pacer (memory pressure stops it, load throttles it). The walk
//!   is the same streaming, descriptor-relative, no-follow walk the
//!   foreground uses (memory bounded by the directory depth), with no entry
//!   limit, a 30-minute backstop, and an early stop once the bytes it has
//!   seen pass the budget, which is already a conclusive answer.
//! - **No lease.** The cache lease makes the foreground inventory exact and
//!   keeps rotation and recovery away from running steps. The measurement
//!   holds no lease, so it never makes a `prepare` wait. Entries that vanish
//!   or are replaced while it walks are skipped (a running step may be
//!   writing the store), so its total is an estimate. A rotation or a root
//!   change during the walk is caught by comparing the root's and the
//!   store's identity before and after; such a walk is discarded. The result
//!   only decides whether a rotation is due: the rotation itself still runs
//!   in the foreground, under the lease, through the existing journalled path
//!   and its identity proofs.
//! - **Use.** The next foreground budget check that finds a result bound to
//!   the same root and store identity, younger than one budget-check interval
//!   (the same trust a foreground measurement gets before it is repeated),
//!   decides from it instead of running its own inventory: over budget
//!   rotates, within budget is conclusive. A within-budget result that is
//!   used queues a refresh, so the next hourly check finds a fresh one.
//! - **Restart, not resume.** An interrupted walk is restarted from scratch
//!   after a back-off. Directory order is not stable across opens and the
//!   tree changes between runs, so a resumed partial sum would add up two
//!   different snapshots; the walk reads only metadata, so a restart is cheap.
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
/// Broker metadata key of the last conclusive measurement (JSON).
pub(crate) const RESULT_KEY: &str = "preparation.shared_cache_measurement.result";
pub(crate) const REQUEST_SCHEMA: &str = "aethyme.preparation-cache-measurement-request/v1";
pub(crate) const RESULT_SCHEMA: &str = "aethyme.preparation-cache-measurement/v1";
/// A result older than this is not used. It is the budget-check interval:
/// a foreground measurement is trusted for exactly that long before the
/// check repeats, and a background one gets the same trust, no more.
pub(crate) const RESULT_MAX_AGE_MS: i64 = SHARED_CACHE_BUDGET_CHECK_INTERVAL_MS;
/// Back-off after an interrupted walk before a worker restarts it.
pub(crate) const RETRY_AFTER_INTERRUPTION_MS: i64 = 10 * 60_000;
/// Back-off when another repository's measurement holds the host slot.
pub(crate) const RETRY_AFTER_HOST_BUSY_MS: i64 = 60_000;
/// Wall-clock backstop for one walk. Not a measurement limit: a store that
/// takes longer than this at background priority is reported, not guessed.
pub(crate) const MAX_WALK_DURATION: Duration = Duration::from_secs(30 * 60);
/// Entries between two pacer checks. A metadata walk visits tens of
/// thousands of entries a second, so this is a fraction of a second.
pub(crate) const PACING_BATCH_ENTRIES: u64 = 4096;
/// Host-wide advisory lock: one measurement per machine at a time.
pub(crate) const HOST_LOCK_FILE: &str = "preparation-cache-measurement.lock";
const ROTATION_PROBE_MAX_ENTRIES: usize = 4096;
const ROTATION_PROBE_BUDGET: Duration = Duration::from_secs(2);
const CAS_ATTEMPTS: usize = 4;

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
    /// `inconclusive_entry_limit`), or `refresh`.
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
}

/// The answer a completed measurement gives.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementOutcome {
    WithinBudget,
    OverBudget,
}

impl MeasurementOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WithinBudget => "within_budget",
            Self::OverBudget => "over_budget",
        }
    }
}

/// The last conclusive background measurement. Interrupted walks are kept in
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
    /// because the bytes it had seen passed the budget (`bytes` is then a
    /// lower bound).
    pub complete: bool,
    pub bytes: u64,
    pub entries: u64,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub duration_ms: u64,
    pub entries_per_second: u64,
    pub bytes_per_second: u64,
    pub pacer_memory_pause_ms: u64,
    pub pacer_load_pause_ms: u64,
    pub attempt: u64,
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

    /// Whether this record answers the budget question for `budget`.
    fn decides(&self, budget: u64) -> bool {
        self.bytes > budget || (self.complete && self.bytes <= budget)
    }
}

/// `submit prepare status --json`: the repository's background measurement.
#[derive(Debug, Clone, Serialize)]
pub struct SharedCacheMeasurementStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<SharedCacheMeasurementRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<SharedCacheMeasurement>,
    /// Whether `result` is young enough for the next budget check to decide
    /// from it; root and store identity are checked then.
    pub result_fresh: bool,
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
    HostBusy,
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

    /// The pending background measurement request, if any.
    pub(crate) fn shared_cache_measurement_request(&self) -> Option<SharedCacheMeasurementRequest> {
        self.read_measurement_request().1
    }

    /// The last conclusive background measurement, if any.
    pub(crate) fn shared_cache_measurement_result(&self) -> Option<SharedCacheMeasurement> {
        parse(
            self.store_ref()
                .meta_get(RESULT_KEY)
                .ok()
                .flatten()
                .as_deref(),
        )
    }

    /// Whether a request is waiting and not backing off. One metadata read,
    /// cheap enough for worker admission on every broker open.
    pub(crate) fn shared_cache_measurement_due(&self, now: i64) -> bool {
        self.shared_cache_measurement_request()
            .is_some_and(|request| request.retry_after_ms.is_none_or(|after| after <= now))
    }

    /// The status view, or `None` when nothing was ever queued or measured.
    pub(crate) fn shared_cache_measurement_status(&self) -> Option<SharedCacheMeasurementStatus> {
        let request = self.shared_cache_measurement_request();
        let result = self.shared_cache_measurement_result();
        if request.is_none() && result.is_none() {
            return None;
        }
        let now = now_ms();
        let result_fresh = result
            .as_ref()
            .is_some_and(|result| (0..RESULT_MAX_AGE_MS).contains(&result.age_ms(now)));
        Some(SharedCacheMeasurementStatus {
            request,
            result,
            result_fresh,
        })
    }

    /// A conclusive measurement of `repository`'s store under `cache_root`
    /// that is young enough and bound to the root's current identity. The
    /// store's identity is checked later, under the lease.
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
        if !(0..RESULT_MAX_AGE_MS).contains(&result.age_ms(now)) || !result.decides(budget) {
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
    /// store under `cache_root`. Returns whether the request is durable.
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
                    pending.reason = reason.to_string();
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
            let decided_by = SharedCacheMeasurementUse {
                outcome: measurement.outcome,
                bytes: measurement.bytes,
                entries: measurement.entries,
                complete: measurement.complete,
                completed_at_ms: measurement.completed_at_ms,
                age_ms: measurement.age_ms(now),
            };
            // A rotated store needs no refresh; a kept one is still too large
            // to measure in the foreground, so the next check needs a new one.
            let queued = (outcome == "within_budget_background"
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
        let Some(raw) = self.store_ref().meta_get(RESULT_KEY).ok().flatten() else {
            return;
        };
        let Some(mut result) = parse::<SharedCacheMeasurement>(Some(raw.as_str())) else {
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

    /// Update the repeated-inconclusive record after a background walk: a
    /// conclusive result resets it (the status notice clears), an interrupted
    /// one is noted in it so the notice says why it is still unmeasured.
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
        let now = now_ms();
        let (raw, Some(mut request)) = self.read_measurement_request() else {
            return Ok(MeasurementLaneOutcome::NotDue);
        };
        if request.retry_after_ms.is_some_and(|after| after > now) {
            return Ok(MeasurementLaneOutcome::NotDue);
        }
        let _lane = crate::phase_telemetry::span("preparation_cache_measurement_lane");
        let _host_slot = match acquire_host_measurement_slot(host_lock_directory) {
            HostMeasurementSlot::Held(lock) => Some(lock),
            HostMeasurementSlot::Unavailable => None,
            HostMeasurementSlot::Busy => {
                crate::phase_telemetry::add(
                    "preparation_cache_measurement_host_busy",
                    Duration::ZERO,
                );
                request.retry_after_ms = Some(now.saturating_add(RETRY_AFTER_HOST_BUSY_MS));
                let written = self.replace_measurement_request(raw.as_deref(), Some(&request));
                crate::warn_unrecorded(
                    "defer the shared-cache background measurement",
                    written.map(|_| ()),
                );
                return Ok(MeasurementLaneOutcome::HostBusy);
            }
        };
        request.attempts = request.attempts.saturating_add(1);
        request.retry_after_ms = None;
        let attempt = request.attempts;
        if !self
            .replace_measurement_request(raw.as_deref(), Some(&request))
            .unwrap_or(false)
        {
            // A prepare changed the request meanwhile: the next pass reads it.
            return Ok(MeasurementLaneOutcome::NotDue);
        }
        let started_raw = serde_json::to_string(&Some(&request)).ok();
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
        let started_at_ms = now_ms();
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
        crate::phase_telemetry::add("preparation_cache_measurement_walk", duration);
        crate::phase_telemetry::add(
            "preparation_cache_measurement_pacer_memory_pause",
            Duration::from_millis(memory_pause_ms),
        );
        crate::phase_telemetry::add(
            "preparation_cache_measurement_pacer_load_wait",
            Duration::from_millis(load_pause_ms),
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
                let result = SharedCacheMeasurement {
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
                    started_at_ms,
                    completed_at_ms,
                    duration_ms,
                    entries_per_second: per_second(measured.entries),
                    bytes_per_second: per_second(measured.bytes),
                    pacer_memory_pause_ms: memory_pause_ms,
                    pacer_load_pause_ms: load_pause_ms,
                    attempt,
                    used_at_ms: None,
                    times_used: 0,
                };
                let value = serde_json::to_string(&result)?;
                self.store().meta_set(RESULT_KEY, &value)?;
                // Clear the request unless a prepare re-queued it meanwhile.
                let cleared = self.replace_measurement_request(started_raw.as_deref(), None);
                crate::warn_unrecorded(
                    "clear the shared-cache background measurement request",
                    cleared.map(|_| ()),
                );
                crate::phase_telemetry::add(
                    match result.outcome {
                        MeasurementOutcome::WithinBudget => {
                            "preparation_cache_measurement_completed_within_budget"
                        }
                        MeasurementOutcome::OverBudget => {
                            "preparation_cache_measurement_completed_over_budget"
                        }
                    },
                    Duration::ZERO,
                );
                if result.outcome == MeasurementOutcome::OverBudget
                    && let Some(binding) = binding
                {
                    self.make_shared_cache_budget_check_due(&binding.path, completed_at_ms);
                }
                self.note_background_measurement_on_inconclusive_record(
                    Some(&result),
                    format!(
                        "settled by a background measurement: {} ({} bytes, {} entries{})",
                        result.outcome.as_str(),
                        result.bytes,
                        result.entries,
                        if result.complete {
                            ""
                        } else {
                            ", stopped once over budget"
                        }
                    ),
                );
                Ok(MeasurementLaneOutcome::Completed(result))
            }
            Err(interrupted) => {
                crate::phase_telemetry::add(interrupted.reason.counter(), Duration::ZERO);
                let interruption = SharedCacheMeasurementInterruption {
                    reason: interrupted.reason,
                    detail: interrupted.detail,
                    at_ms: completed_at_ms,
                    attempt,
                    entries: interrupted.entries,
                    bytes: interrupted.bytes,
                    duration_ms,
                    pacer_memory_pause_ms: memory_pause_ms,
                    pacer_load_pause_ms: load_pause_ms,
                };
                let next = (!interrupted.reason.drops_request()).then(|| {
                    let mut retained = request.clone();
                    retained.retry_after_ms =
                        Some(completed_at_ms.saturating_add(RETRY_AFTER_INTERRUPTION_MS));
                    retained.last_interruption = Some(interruption.clone());
                    retained
                });
                let written =
                    self.replace_measurement_request(started_raw.as_deref(), next.as_ref());
                crate::warn_unrecorded(
                    "record the interrupted shared-cache background measurement",
                    written.map(|_| ()),
                );
                self.note_background_measurement_on_inconclusive_record(
                    None,
                    format!(
                        "background measurement attempt {attempt} interrupted ({}) after {} entries and {} bytes{}",
                        interruption.reason.as_str(),
                        interruption.entries,
                        interruption.bytes,
                        if interrupted.reason.drops_request() {
                            "; the next inconclusive check queues the current root"
                        } else {
                            "; it restarts after a back-off"
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

enum HostMeasurementSlot {
    Held(crate::file_lock::ExclusiveFileLock),
    /// No host state directory could hold the lock: proceed unserialised
    /// rather than strand the request (the per-repository worker lock still
    /// holds).
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
}

impl Interrupted {
    fn new(reason: MeasurementInterruptionReason, detail: Option<String>) -> Self {
        Self {
            reason,
            detail,
            entries: 0,
            bytes: 0,
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
}

struct MeasureRequest<'a> {
    cache_root: &'a Path,
    binding_identity: Option<&'a [u8]>,
    expected_root_identity: Option<&'a str>,
    repository: &'a str,
    budget: u64,
    resource_db_path: Option<&'a Path>,
    max_elapsed: Duration,
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
    let deadline = Instant::now() + max_elapsed;
    let seen = Cell::new((0_u64, 0_u64));
    let memory_stop = Cell::new(false);
    let mut on_entry = |entries: u64, bytes: u64| -> Result<(), PreparationError> {
        seen.set((entries, bytes));
        // At the first entry and then once per batch.
        if (entries.saturating_sub(1)).is_multiple_of(PACING_BATCH_ENTRIES)
            && unattended_pacing::before_unattended_scan_batch(Some(deadline))
                == unattended_pacing::PressureVerdict::MemoryBackoff
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
        Some(budget),
        SharedCacheScanConcurrency::Concurrent,
        |_| {},
        &mut |_| Ok(()),
        &mut on_entry,
    );
    let (entries_seen, bytes_seen) = seen.get();
    let interrupted = |reason, detail| Interrupted {
        reason,
        detail,
        entries: entries_seen,
        bytes: bytes_seen,
    };
    let (inventory, entries) = match walked {
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
    let measured = |outcome, complete, bytes| Measured {
        root_identity: Some(root_before.clone()),
        store_identity: store_before.clone(),
        outcome,
        complete,
        bytes,
        entries,
    };
    match inventory {
        SharedCacheInventory::Absent => Ok(measured(MeasurementOutcome::WithinBudget, true, 0)),
        SharedCacheInventory::Complete { bytes } if bytes <= budget => {
            Ok(measured(MeasurementOutcome::WithinBudget, true, bytes))
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

    const REPOSITORY: &str = "repository-identity";

    struct Fixture {
        _repository: tempfile::TempDir,
        _storage: crate::test_storage::TestStorage,
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

        /// Let the request's back-off elapse, as time passing would.
        fn expire_retry(&mut self) {
            let mut request = self.broker.shared_cache_measurement_request().unwrap();
            request.retry_after_ms = Some(0);
            let value = serde_json::to_string(&Some(&request)).unwrap();
            self.broker.store().meta_set(REQUEST_KEY, &value).unwrap();
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
        let store = cache_root.join(REPOSITORY);
        fs::create_dir_all(store.join("nested")).unwrap();
        for index in 0..files {
            fs::write(
                store.join("nested").join(format!("file-{index}")),
                vec![0_u8; file_bytes],
            )
            .unwrap();
        }
        Fixture {
            _repository: repository,
            _storage: storage,
            broker,
            cache,
            cache_root,
            host: tempfile::tempdir().unwrap(),
        }
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

        // A second inconclusive check joins the pending request.
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

        // A conclusive foreground check queues nothing.
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
        assert!(!status.result_fresh);
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
        assert_eq!(result.attempt, 1);
        assert_eq!(result.store_identity, identity_digest(&fixture.store()));
        assert!(
            fixture.broker.shared_cache_measurement_request().is_none(),
            "a completed measurement consumes its request"
        );
        let fresh = fixture
            .broker
            .fresh_shared_cache_measurement(REPOSITORY, &fixture.cache_root, 10_000, now_ms())
            .expect("a fresh result bound to the root is used");
        assert_eq!(fresh, result);

        let budget = SharedCacheBudget {
            max_bytes: 10_000,
            ..SharedCacheBudget::default()
        };
        let db = fixture
            .cache_root
            .with_file_name(".aethyme-test-host-resources.sqlite3");
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
        let decided_by = report.decided_by.expect("the decision source is reported");
        assert_eq!(decided_by.outcome, MeasurementOutcome::WithinBudget);
        assert_eq!(decided_by.bytes, 1_000);
        assert_eq!(
            report.queued.as_deref(),
            Some("refresh"),
            "a kept store is measured again before the next check"
        );
        let stored = fixture.broker.shared_cache_measurement_result().unwrap();
        assert_eq!(stored.times_used, 1);
        assert!(stored.used_at_ms.is_some());
    }

    #[test]
    fn an_over_budget_measurement_stops_early_and_rotates_through_the_existing_path() {
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
        let result = completed(fixture.run_lane());
        assert_eq!(result.outcome, MeasurementOutcome::OverBudget);
        assert!(!result.complete, "the walk stops once over budget");
        assert_eq!(result.bytes, 200, "it stopped at the second file");
        assert!(result.entries <= 3, "{} entries", result.entries);
        // The next prepare that finds the cache free checks the budget.
        let stamped: i64 = fixture
            .broker
            .store()
            .meta_get(&stamp)
            .unwrap()
            .unwrap()
            .parse()
            .unwrap();
        assert!(stamped <= checked_at - SHARED_CACHE_BUDGET_CHECK_INTERVAL_MS);

        let budget = SharedCacheBudget {
            max_bytes: 150,
            ..SharedCacheBudget::default()
        };
        let db = fixture
            .cache_root
            .with_file_name(".aethyme-test-host-resources.sqlite3");
        let guard = activity();
        let rotation = shared_cache_budget_with_background(
            fixture.budget_request(&budget, &db),
            Some(&result),
            &mut |_| Ok(()),
            no_time(),
        )
        .unwrap()
        .expect("an over-budget measurement rotates the store");
        assert!(rotation.removed);
        assert_eq!(
            rotation.rotation_reason,
            SharedCacheRotationReason::BudgetExceeded
        );
        assert_eq!(rotation.bytes_before, None);
        assert_eq!(rotation.observed_bytes_lower_bound, 200);
        assert!(fixture.store().is_dir());
        assert_eq!(fs::read_dir(fixture.store()).unwrap().count(), 0);
        assert_eq!(
            inventory_summary(guard).outcome,
            Some("over_budget_background")
        );
    }

    #[test]
    fn a_result_for_a_changed_store_or_root_is_ignored() {
        let mut fixture = fixture(4, 100);
        fixture.queue(10_000);
        let _pacing = pacer(vec![calm()]);
        let result = completed(fixture.run_lane());

        // The store replaced (as a rotation and a later prepare would): the
        // old directory still exists, so the new one has another inode.
        let store = fixture.store();
        fs::rename(&store, fixture.cache_root.join("retired")).unwrap();
        fs::create_dir(&store).unwrap();
        let budget = SharedCacheBudget {
            max_bytes: 10_000,
            ..SharedCacheBudget::default()
        };
        let db = fixture
            .cache_root
            .with_file_name(".aethyme-test-host-resources.sqlite3");
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

        // Another root, another repository, a changed root identity, an
        // aged result, or a budget the result cannot decide: not used.
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
                .is_some(),
            "a complete 400-byte result also decides a 100-byte budget (over it)"
        );
        let write = |fixture: &mut Fixture, result: &SharedCacheMeasurement| {
            fixture
                .broker
                .store()
                .meta_set(RESULT_KEY, &serde_json::to_string(result).unwrap())
                .unwrap();
        };
        let mut remounted = result.clone();
        remounted.root_identity = Some("0".repeat(32));
        write(&mut fixture, &remounted);
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement(REPOSITORY, &fixture.cache_root, 10_000, now)
                .is_none()
        );
        let mut aged = result.clone();
        aged.completed_at_ms = now - RESULT_MAX_AGE_MS;
        write(&mut fixture, &aged);
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement(REPOSITORY, &fixture.cache_root, 10_000, now)
                .is_none()
        );
        let mut partial = result.clone();
        partial.complete = false;
        partial.bytes = 300;
        write(&mut fixture, &partial);
        assert!(
            fixture
                .broker
                .fresh_shared_cache_measurement(REPOSITORY, &fixture.cache_root, 10_000, now)
                .is_none(),
            "a lower bound under budget decides nothing"
        );
    }

    #[test]
    fn memory_pressure_pauses_the_walk_and_a_persistent_one_interrupts_it() {
        let mut fixture = fixture(3, 100);
        fixture.queue(10_000);
        {
            // Two pressured samples, then calm: the walk waits and finishes.
            let _pacing = pacer(vec![memory_pressure(), memory_pressure(), calm()]);
            let result = completed(fixture.run_lane());
            assert!(
                result.pacer_memory_pause_ms >= 2_000,
                "{} ms paused",
                result.pacer_memory_pause_ms
            );
            assert_eq!(result.outcome, MeasurementOutcome::WithinBudget);
        }

        fixture.queue(10_000);
        let _pacing = pacer(vec![memory_pressure()]);
        let interruption = interrupted(fixture.run_lane());
        assert_eq!(
            interruption.reason,
            MeasurementInterruptionReason::MemoryPressure
        );
        assert!(interruption.pacer_memory_pause_ms > 0);
        let request = fixture
            .broker
            .shared_cache_measurement_request()
            .expect("the request stays durable");
        assert_eq!(request.attempts, 1);
        assert!(request.retry_after_ms.is_some_and(|after| after > now_ms()));
        assert_eq!(request.last_interruption, Some(interruption));
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
    fn two_workers_never_measure_at_once() {
        let mut fixture = fixture(3, 100);
        fixture.queue(10_000);
        let _pacing = pacer(vec![calm()]);
        // Another repository's worker holds this host's measurement slot.
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
        // Still inconclusive in the foreground: the note stays with it.
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
    fn a_concurrent_walk_skips_entries_that_vanish_and_an_exclusive_one_refuses() {
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
                    // A writer removes the directory between listing and open.
                    if matches!(target, SharedCacheScanOpenTarget::Child) && !removed {
                        removed = true;
                        let _ = fs::rename(&nested, store.join("..").join("moved-away"));
                    }
                },
                &mut |_| Ok(()),
                &mut |_, _| Ok(()),
            )
        };
        let (inventory, _) = walk(SharedCacheScanConcurrency::Concurrent).unwrap();
        assert_eq!(inventory, SharedCacheInventory::Complete { bytes: 0 });
        fs::rename(fixture.cache_root.join("moved-away"), store.join("nested")).unwrap();
        assert!(walk(SharedCacheScanConcurrency::Exclusive).is_err());
    }
}
