//! The per-repository total size bound on recovery archives.
//!
//! The retirement lane writes new archives on the pool's volume (see
//! `Broker::retirement_archive_root`); older ones stay readable where they
//! were written, in host state. Both count against one bound,
//! `[cleanup] recovery_archive_bound_gib` (default 20, `0` off). Each
//! retirement run measures them (bounded by its deadline) and, over the
//! bound, removes the oldest archives whose content has fully landed (every
//! commit they record reached a delivery target and they hold no uncommitted
//! change), past their minimum retention, until the total is within the
//! bound. An archive holding work that exists nowhere else is never removed
//! for size: the run counts it and `storage status` and `broker status`
//! advise instead.
//!
//! Age: a fully landed archive on the pool's volume older than
//! `[retention] recovery_archive_days` (default 90) is removed by the same
//! pass (host state archives keep expiring through the storage worker). An archive holding unlanded commits or uncommitted
//! changes is never removed by age here; removing one takes a reviewed
//! `broker gc apply`.
//!
//! An archive carrying its owner's keep marker (`storage keep-archive`,
//! `<archive>.aethyme-keep` beside it) is never removed by this pass.
//!
//! Pacing: listing and sizing read only directory entries, manifests and
//! session records, within one budget; landing checks, removal proofs and
//! removals have a budget of their own. What a pass could not finish is
//! carried to the next in a per-repository state file beside the usage
//! record: measured sizes, a listing cursor, size walks to retry, and
//! archives whose removal proof ran out of budget or failed, which later
//! passes rotate past until their retry is due. Only the archive about to
//! be removed is fully proven, under the lifecycle claim.
//!
//! What it measured is recorded per repository in the storage ledger
//! (`storage-ledger-recovery-archives/<sha256 of the repository root>.json`),
//! with bytes and counts by location, and shown by `aethyme storage status`.

use super::*;
use std::time::{Duration, Instant};

use crate::storage_ledger::RecoveryArchiveUsage as ArchiveBoundUsage;
use crate::storage_ledger::{ArchiveBoundState, MeasuredArchiveSize};

/// The longest listing and sizing may take in one pass.
const ARCHIVE_BOUND_BUDGET: Duration = Duration::from_secs(20);
/// The longest landing checks, removal proofs and removals may take in one
/// pass, on top of listing and sizing: a pass whose sizing used its whole
/// budget still removes what it can.
const ARCHIVE_EVICTION_BUDGET: Duration = Duration::from_secs(60);
/// The removal of one proven archive may outlast the eviction budget by
/// this much: a removal stopped half way leaves an archive that no longer
/// proves and can then only be removed by hand.
const ARCHIVE_REMOVAL_GRACE: Duration = Duration::from_secs(30);
/// How long an archive whose removal proof ran out of budget (or failed)
/// is passed over before it is retried, first and with a fresh budget.
const PROOF_RETRY_AFTER_MS: i64 = 60 * 60 * 1_000;
/// Sessions named one by one in the advice, at most.
const UNIQUE_SESSIONS_NAMED: usize = 20;

#[cfg(test)]
thread_local! {
    /// Test hooks: a clock offset for the pass, a bound replacing the
    /// configured one, and how many of the next removal proofs start
    /// already out of budget.
    pub(super) static CLOCK_SKEW_MS_FOR_TEST: std::cell::Cell<i64> =
        const { std::cell::Cell::new(0) };
    pub(super) static BOUND_BYTES_FOR_TEST: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
    pub(super) static EXPIRED_PROOFS_FOR_TEST: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

fn pass_now_ms() -> i64 {
    crate::clock::epoch_ms().saturating_add(clock_skew_ms())
}

#[cfg(test)]
fn clock_skew_ms() -> i64 {
    CLOCK_SKEW_MS_FOR_TEST.with(std::cell::Cell::get)
}

#[cfg(not(test))]
fn clock_skew_ms() -> i64 {
    0
}

/// What one bound pass did, for phase telemetry.
#[derive(Debug, Default)]
struct PassStats {
    /// Archives classified from metadata.
    listed: usize,
    /// Archives the listing did not reach before the deadline (the next
    /// listing resumes after the last one reached).
    skipped_by_budget: usize,
    /// Archives not listed this pass but counted from an earlier size.
    counted_from_earlier_size: usize,
    /// Sizes taken from earlier passes, walked now, stopped by the budget
    /// (retried next run), or failed (never recorded).
    size_cached: usize,
    size_measured: usize,
    size_skipped_by_budget: usize,
    size_failed: usize,
    /// Git landing checks run, and archives left unjudged by the budget.
    landing_checks: usize,
    landing_unjudged: usize,
    /// Full removal proofs started, their outcome, and the archives
    /// passed over because an earlier proof of theirs ran out of budget
    /// or failed and their retry is not due yet.
    proofs: usize,
    evicted: usize,
    proof_timeouts: usize,
    proof_failures: usize,
    proof_deferred_skipped: usize,
    proof_retries: usize,
    proof_ms: u64,
    /// Phases (listing, sizing, eviction) stopped by their deadline.
    budget_stops: usize,
}

impl PassStats {
    fn record(&self) {
        if self.budget_stops > 0 {
            crate::phase_telemetry::add("recovery_archive_bound_budget_stop", Duration::ZERO);
        }
        crate::phase_telemetry::attach(
            "recovery_archive_bound_pass",
            serde_json::json!({
                "listed": self.listed,
                "listing_skipped_by_budget": self.skipped_by_budget,
                "counted_from_earlier_size": self.counted_from_earlier_size,
                "size_cached": self.size_cached,
                "size_measured": self.size_measured,
                "size_skipped_by_budget": self.size_skipped_by_budget,
                "size_failed": self.size_failed,
                "landing_checks": self.landing_checks,
                "landing_unjudged": self.landing_unjudged,
                "proofs": self.proofs,
                "evicted": self.evicted,
                "proof_timeouts": self.proof_timeouts,
                "proof_failures": self.proof_failures,
                "proof_deferred_skipped": self.proof_deferred_skipped,
                "proof_retries": self.proof_retries,
                "proof_ms": self.proof_ms,
                "budget_stops": self.budget_stops,
            }),
        );
    }
}

struct Measured {
    archive: crate::cleanup_resolve::OwnedRecoveryArchive,
    bytes: u64,
    /// Old enough, not kept and holding no uncommitted change: removable
    /// once its commits are shown landed.
    candidate: bool,
    /// On the pool's volume and older than `recovery_archive_days`.
    aged_out: bool,
    /// The Git landing verdict, once judged (judged at most once a pass).
    landed: Option<bool>,
}

/// How a size walk ended.
enum SizeWalk {
    Measured(u64),
    OutOfBudget,
    Failed,
}

fn walk_size(path: &Path, deadline: Instant) -> SizeWalk {
    if Instant::now() >= deadline {
        return SizeWalk::OutOfBudget;
    }
    match crate::disk_headroom::directory_usage_bounded(path, deadline) {
        Some(usage) => SizeWalk::Measured(usage.bytes),
        None if Instant::now() >= deadline => SizeWalk::OutOfBudget,
        None => SizeWalk::Failed,
    }
}

impl Broker {
    /// Measure this repository's recovery archives against the bound and
    /// remove the oldest fully landed ones while over it (see the module
    /// documentation). Never removes an archive holding unique work.
    pub(crate) fn enforce_recovery_archive_bound(&self, deadline: Instant) -> ArchiveBoundUsage {
        let _span = crate::phase_telemetry::span("recovery_archive_bound_pass");
        let scan_deadline = deadline.min(Instant::now() + ARCHIVE_BOUND_BUDGET);
        let bound_bytes = crate::auto_cleanup::recovery_archive_bound_bytes(self.main_root());
        #[cfg(test)]
        let bound_bytes = BOUND_BYTES_FOR_TEST
            .with(std::cell::Cell::get)
            .unwrap_or(bound_bytes);
        let mut usage = ArchiveBoundUsage {
            bound_bytes,
            observed_at_ms: crate::clock::epoch_ms(),
            ..ArchiveBoundUsage::default()
        };
        // What earlier passes left: sizes, deferred walks and proofs, and
        // where an interrupted listing stopped.
        let host_state = self
            .storage_context()
            .host_state_dir
            .clone()
            .or_else(|| crate::host_state::resolve_host_state_dir().map(|(path, _)| path));
        let repository = self.main_root().to_string_lossy().into_owned();
        let previous = host_state
            .as_deref()
            .map(|host| crate::storage_ledger::archive_bound_state(host, &repository))
            .unwrap_or_default();
        // Listing and classifying read only directory entries, manifests
        // and session records: no archive content is read or hashed here.
        // The one archive about to be removed is fully proven, under the
        // lifecycle claim, in `evict_landed_archive`.
        let listing_span = crate::phase_telemetry::span("recovery_archive_bound_listing");
        let scan = match self.recovery_archive_listing(
            scan_deadline,
            previous.listing_cursor.as_deref().map(Path::new),
        ) {
            Ok(scan) => scan,
            Err(error) => {
                usage.partial = true;
                crate::warn_unrecorded::<(), _>("scan recovery archives for the bound", Err(error));
                return usage;
            }
        };
        drop(listing_span);
        let mut stats = PassStats {
            listed: scan.owned.len() + scan.unowned.len(),
            skipped_by_budget: scan.skipped_by_budget,
            ..PassStats::default()
        };
        let mut state = ArchiveBoundState {
            listing_cursor: if scan.not_listed.is_empty() {
                None
            } else {
                scan.last_listed
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .or_else(|| previous.listing_cursor.clone())
            },
            ..ArchiveBoundState::default()
        };
        if scan.skipped_by_budget > 0 {
            usage.partial = true;
            stats.budget_stops += 1;
        }
        let pool_root = self.pool_recovery_archive_root(false);
        usage.pool.root = pool_root
            .as_ref()
            .map(|root| root.to_string_lossy().into_owned());
        usage.host_state.root = Some(scan.root.to_string_lossy().into_owned());
        let in_pool = |path: &Path| {
            pool_root
                .as_ref()
                .is_some_and(|root| path.starts_with(root))
        };
        let key = |path: &Path| path.to_string_lossy().into_owned();

        // Sizing. A complete archive the broker owns never changes, so its
        // size, once walked, is kept (by path and creation time). Archives
        // not walked before are walked in listing order, the ones whose
        // walk ran out of budget last time after the others; a walk stopped
        // by the budget is retried next run, a failed one is not recorded.
        let sizing_span = crate::phase_telemetry::span("recovery_archive_bound_sizing");
        let mut owned = scan.owned;
        owned.sort_by_key(|archive| {
            previous
                .size_deferred_at_ms
                .get(&key(&archive.path))
                .copied()
                .unwrap_or(i64::MIN)
        });
        let mut sized = Vec::new();
        for archive in owned {
            let path_key = key(&archive.path);
            if let Some(known) = previous
                .sizes
                .get(&path_key)
                .filter(|known| known.created_at_ms == archive.created_at_ms)
            {
                stats.size_cached += 1;
                state.sizes.insert(path_key, *known);
                sized.push((archive, known.bytes));
                continue;
            }
            match walk_size(&archive.path, scan_deadline) {
                SizeWalk::Measured(bytes) => {
                    stats.size_measured += 1;
                    state.sizes.insert(
                        path_key,
                        MeasuredArchiveSize {
                            created_at_ms: archive.created_at_ms,
                            bytes,
                        },
                    );
                    sized.push((archive, bytes));
                }
                SizeWalk::OutOfBudget => {
                    stats.size_skipped_by_budget += 1;
                    usage.partial = true;
                    state.size_deferred_at_ms.insert(path_key, pass_now_ms());
                }
                SizeWalk::Failed => {
                    stats.size_failed += 1;
                    usage.partial = true;
                }
            }
        }
        // Archives the broker cannot vouch for count, are never removed,
        // and are walked afresh each pass (their content may change).
        for (path, _) in &scan.unowned {
            match walk_size(path, scan_deadline) {
                SizeWalk::Measured(bytes) => {
                    stats.size_measured += 1;
                    let location = if in_pool(path) {
                        &mut usage.pool
                    } else {
                        &mut usage.host_state
                    };
                    location.archives += 1;
                    location.bytes = location.bytes.saturating_add(bytes);
                }
                SizeWalk::OutOfBudget => {
                    stats.size_skipped_by_budget += 1;
                    usage.partial = true;
                }
                SizeWalk::Failed => {
                    stats.size_failed += 1;
                    usage.partial = true;
                }
            }
        }
        // Archives this listing did not reach count with the size an
        // earlier pass measured, so a long listing does not hide them from
        // the total; they are not candidates this pass.
        for path in &scan.not_listed {
            if let Some(known) = previous.sizes.get(&key(path)) {
                stats.counted_from_earlier_size += 1;
                state.sizes.insert(key(path), *known);
                let location = if in_pool(path) {
                    &mut usage.pool
                } else {
                    &mut usage.host_state
                };
                location.archives += 1;
                location.bytes = location.bytes.saturating_add(known.bytes);
            }
        }
        if stats.size_skipped_by_budget > 0 {
            stats.budget_stops += 1;
        }
        drop(sizing_span);

        let now = pass_now_ms();
        let retention_days = crate::retention::load_retention_policy(self.main_root())
            .map_or(90, |policy| policy.recovery_archive_days);
        let mut measured = Vec::new();
        for (archive, bytes) in sized {
            let location = if in_pool(&archive.path) {
                &mut usage.pool
            } else {
                &mut usage.host_state
            };
            location.archives += 1;
            location.bytes = location.bytes.saturating_add(bytes);
            let minimum_age_ms = archive
                .minimum_archive_retention_ms
                .unwrap_or(crate::retention::MIN_RECOVERY_ARCHIVE_RETENTION_MS);
            let old_enough = now
                .checked_sub(archive.created_at_ms)
                .and_then(|age| u64::try_from(age).ok())
                .is_some_and(|age| age >= minimum_age_ms);
            // An owner's keep marker (`storage keep-archive`) exempts it, as
            // it does from the storage worker's expiry.
            let kept = crate::storage_ledger::keep_marker(&archive.path).exists();
            if kept {
                crate::phase_telemetry::add("recovery_archive_bound_kept_by_owner", Duration::ZERO);
            }
            // Uncommitted content (and older formats that do not say what
            // they hold) is never landed: no Git check needed to refuse it.
            let candidate = !kept && old_enough && !archive.holds_uncommitted;
            // Host state archives keep expiring through the storage worker
            // (with its notice week); pool ones are this lane's.
            let aged_out = in_pool(&archive.path)
                && retention_days > 0
                && now.saturating_sub(archive.created_at_ms)
                    >= i64::from(retention_days).saturating_mul(86_400_000);
            let landed = archive_never_lands(&archive).then_some(false);
            measured.push(Measured {
                archive,
                bytes,
                candidate,
                aged_out,
                landed,
            });
        }
        let mut total = usage.pool.bytes.saturating_add(usage.host_state.bytes);

        // Eviction, with its own budget: oldest first, an archive whose
        // last proof ran out of budget or failed is passed over until its
        // retry is due, then tried first with the whole budget.
        let evict_span = crate::phase_telemetry::span("recovery_archive_bound_eviction");
        let evict_deadline = deadline.min(Instant::now() + ARCHIVE_EVICTION_BUDGET);
        measured.sort_by_key(|entry| (entry.archive.created_at_ms, entry.archive.session_id));
        let order = eviction_order(
            &measured
                .iter()
                .map(|entry| {
                    (
                        entry.candidate,
                        previous
                            .proof_deferred_at_ms
                            .get(&key(&entry.archive.path))
                            .map(|at| (now.saturating_sub(*at) >= PROOF_RETRY_AFTER_MS, *at)),
                    )
                })
                .collect::<Vec<_>>(),
        );
        for entry in &measured {
            let path_key = key(&entry.archive.path);
            if let Some(at) = previous.proof_deferred_at_ms.get(&path_key) {
                // Carried over until retried (or the archive is gone).
                state.proof_deferred_at_ms.insert(path_key, *at);
            }
        }
        stats.proof_deferred_skipped = measured
            .iter()
            .enumerate()
            .filter(|(index, entry)| {
                entry.candidate
                    && !order.contains(index)
                    && previous
                        .proof_deferred_at_ms
                        .contains_key(&key(&entry.archive.path))
            })
            .count();
        let mut removed = vec![false; measured.len()];
        for index in order {
            if Instant::now() >= evict_deadline {
                stats.budget_stops += 1;
                break;
            }
            let needed =
                measured[index].aged_out || (usage.bound_bytes > 0 && total > usage.bound_bytes);
            if !needed {
                continue;
            }
            let landed = match measured[index].landed {
                Some(landed) => landed,
                None => {
                    stats.landing_checks += 1;
                    let landed = self.recovery_archive_landed(&measured[index].archive);
                    measured[index].landed = Some(landed);
                    landed
                }
            };
            if !landed {
                continue;
            }
            let entry = &measured[index];
            let path_key = key(&entry.archive.path);
            let retry = state.proof_deferred_at_ms.remove(&path_key).is_some();
            if retry {
                stats.proof_retries += 1;
            }
            #[cfg(test)]
            let evict_deadline = if EXPIRED_PROOFS_FOR_TEST.with(|left| {
                let expired = left.get() > 0;
                left.set(left.get().saturating_sub(1));
                expired
            }) {
                Instant::now()
            } else {
                evict_deadline
            };
            stats.proofs += 1;
            let started = Instant::now();
            let outcome = self.evict_landed_archive(&entry.archive, evict_deadline);
            let elapsed = started.elapsed();
            stats.proof_ms = stats
                .proof_ms
                .saturating_add(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
            crate::phase_telemetry::add("recovery_archive_bound_evict_proof", elapsed);
            match outcome {
                Ok(()) => {
                    removed[index] = true;
                    stats.evicted += 1;
                    total = total.saturating_sub(entry.bytes);
                    usage.evicted += 1;
                    usage.evicted_bytes = usage.evicted_bytes.saturating_add(entry.bytes);
                    let location = if in_pool(&entry.archive.path) {
                        &mut usage.pool
                    } else {
                        &mut usage.host_state
                    };
                    location.archives = location.archives.saturating_sub(1);
                    location.bytes = location.bytes.saturating_sub(entry.bytes);
                    state.sizes.remove(&path_key);
                    crate::phase_telemetry::add(
                        if entry.aged_out {
                            "recovery_archive_bound_expired"
                        } else {
                            "recovery_archive_bound_evicted"
                        },
                        Duration::ZERO,
                    );
                }
                Err(reason) => {
                    // Passed over until its retry is due, so it never holds
                    // back the archives behind it.
                    if Instant::now() >= evict_deadline {
                        stats.proof_timeouts += 1;
                        crate::phase_telemetry::add(
                            "recovery_archive_bound_proof_timeout",
                            Duration::ZERO,
                        );
                    } else {
                        stats.proof_failures += 1;
                        crate::phase_telemetry::add(
                            "recovery_archive_bound_evict_failed",
                            Duration::ZERO,
                        );
                        crate::warn_unrecorded::<(), _>(
                            "remove a landed recovery archive over the bound",
                            Err(reason),
                        );
                    }
                    state.proof_deferred_at_ms.insert(path_key, pass_now_ms());
                }
            }
        }
        usage.over_bound = usage.bound_bytes > 0 && total > usage.bound_bytes;
        if usage.over_bound {
            // Reuse the verdicts already reached; judge the rest only while
            // the eviction budget lasts.
            let mut unique = Vec::new();
            for (index, entry) in measured.iter_mut().enumerate() {
                if removed[index] {
                    continue;
                }
                let landed = match entry.landed {
                    Some(landed) => Some(landed),
                    None if archive_never_lands(&entry.archive) => Some(false),
                    None if Instant::now() < evict_deadline => {
                        stats.landing_checks += 1;
                        let landed = self.recovery_archive_landed(&entry.archive);
                        entry.landed = Some(landed);
                        Some(landed)
                    }
                    None => None,
                };
                match landed {
                    Some(false) => unique.push(entry.archive.session_id),
                    Some(true) => {}
                    None => {
                        stats.landing_unjudged += 1;
                        usage.partial = true;
                    }
                }
            }
            usage.unique_kept_over_bound = unique.len();
            usage.unique_kept_sessions = unique.into_iter().take(UNIQUE_SESSIONS_NAMED).collect();
            crate::phase_telemetry::add("recovery_archive_bound_over", Duration::ZERO);
        } else {
            crate::phase_telemetry::add("recovery_archive_bound_within", Duration::ZERO);
        }
        drop(evict_span);
        // Keep what earlier passes learned about archives this pass did not
        // reach, while they are still there.
        for (path, known) in previous.sizes {
            if std::fs::symlink_metadata(&path).is_ok() {
                state.sizes.entry(path).or_insert(known);
            }
        }
        for (path, at) in previous.size_deferred_at_ms {
            if !state.sizes.contains_key(&path) && std::fs::symlink_metadata(&path).is_ok() {
                state.size_deferred_at_ms.entry(path).or_insert(at);
            }
        }
        for (path, at) in previous.proof_deferred_at_ms {
            if !measured
                .iter()
                .any(|entry| key(&entry.archive.path) == path)
                && std::fs::symlink_metadata(&path).is_ok()
            {
                state.proof_deferred_at_ms.entry(path).or_insert(at);
            }
        }
        if let Some(host) = host_state.as_deref() {
            crate::warn_unrecorded(
                "record the recovery archive bound state",
                crate::storage_ledger::record_archive_bound_state(host, &repository, &state),
            );
        }
        stats.record();
        crate::phase_telemetry::attach(
            "recovery_archive_bound",
            serde_json::json!({
                "bound_bytes": usage.bound_bytes,
                "pool": usage.pool,
                "host_state": usage.host_state,
                "evicted": usage.evicted,
                "evicted_bytes": usage.evicted_bytes,
                "over_bound": usage.over_bound,
                "unique_kept_over_bound": usage.unique_kept_over_bound,
                "partial": usage.partial,
            }),
        );
        usage
    }

    /// Remove one fully landed archive, re-proving ownership and landing
    /// under the lifecycle claim (a reuse restoring from it waits for the
    /// claim, then finds it gone and recreates from the branch, which holds
    /// everything the archive did). The manifest goes last.
    pub(super) fn evict_landed_archive(
        &self,
        archive: &crate::cleanup_resolve::OwnedRecoveryArchive,
        deadline: Instant,
    ) -> Result<(), String> {
        let Some(_claim) = self
            .try_claim_worktree_lifecycle("retire")
            .map_err(|error| error.to_string())?
        else {
            return Err("another checkout lifecycle operation is running".into());
        };
        if crate::storage_ledger::keep_marker(&archive.path).exists() {
            return Err("kept by the owner's marker".into());
        }
        // The full proof, for this one archive only, bounded by the pass.
        crate::phase_telemetry::add("recovery_archive_bound_verified", Duration::ZERO);
        let current = self.owned_recovery_archive_with(
            &archive.path,
            &self.inspected_recovery_archive_roots(),
            crate::cleanup_resolve::ArchiveProof::Full {
                deadline: Some(deadline),
            },
        )?;
        if current.session_id != archive.session_id
            || current.head != archive.head
            || !self.recovery_archive_landed(&current)
        {
            return Err("no longer a fully landed archive".into());
        }
        // Once proven, the removal gets a short grace past the pass budget:
        // stopping it half way would leave an archive that no longer proves.
        match crate::gc::remove_condemned_tree(
            &current.path,
            Some(crate::cleanup_resolve::MANIFEST_FILE),
            Some(deadline.max(Instant::now() + ARCHIVE_REMOVAL_GRACE)),
        ) {
            Ok(crate::gc::TreeRemoval::Complete) => {}
            Ok(crate::gc::TreeRemoval::Interrupted) => {
                return Err("removal interrupted at the deadline".into());
            }
            Err(error) => return Err(error.to_string()),
        }
        if std::fs::symlink_metadata(&current.path).is_ok() {
            return Err("archive removal could not be verified".into());
        }
        self.release_recovery_archive_prerequisite_refs_after_removal(&current)
    }

    /// Run the bound and record what it measured in the storage ledger.
    pub(crate) fn record_recovery_archive_bound(&self, deadline: Instant) {
        let mut usage = self.enforce_recovery_archive_bound(deadline);
        let Some((host_state, _)) = crate::host_state::resolve_host_state_dir() else {
            return;
        };
        let repository = self.main_root().to_string_lossy().into_owned();
        if let Some(previous) =
            crate::storage_ledger::recovery_archive_usage(&host_state).get(&repository)
        {
            usage.evicted_total = previous.evicted_total.saturating_add(usage.evicted);
            usage.evicted_bytes_total = previous
                .evicted_bytes_total
                .saturating_add(usage.evicted_bytes);
        } else {
            usage.evicted_total = usage.evicted;
            usage.evicted_bytes_total = usage.evicted_bytes;
        }
        crate::warn_unrecorded(
            "record recovery archive usage in the storage ledger",
            crate::storage_ledger::record_recovery_archive_usage(&host_state, &repository, usage),
        );
    }
}

/// Whether an archive can never be shown landed, whatever Git holds: it
/// carries uncommitted content (or is an older format that does not say)
/// or records no root. Refused without a Git check.
fn archive_never_lands(archive: &crate::cleanup_resolve::OwnedRecoveryArchive) -> bool {
    archive.holds_uncommitted || archive.recovery_roots.is_empty()
}

/// The order removals are tried in, from `entries` sorted oldest first as
/// (candidate, deferral): deferral is `None` for an archive with no earlier
/// failed or timed-out proof, else (retry due, when it was deferred). The
/// due deferred archive deferred longest ago goes first, alone, so it gets
/// the whole budget; then every candidate never deferred, oldest first.
/// Other deferred archives are passed over this pass, so one archive that
/// cannot be proven never holds back the ones behind it.
fn eviction_order(entries: &[(bool, Option<(bool, i64)>)]) -> Vec<usize> {
    let retry = entries
        .iter()
        .enumerate()
        .filter_map(|(index, (candidate, deferral))| match deferral {
            Some((true, at)) if *candidate => Some((*at, index)),
            _ => None,
        })
        .min()
        .map(|(_, index)| index);
    retry
        .into_iter()
        .chain(
            entries
                .iter()
                .enumerate()
                .filter(|(_, (candidate, deferral))| *candidate && deferral.is_none())
                .map(|(index, _)| index),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_ledger::ArchiveLocationUsage;

    /// Removals are tried oldest first among never-deferred candidates; a
    /// due deferred archive goes first, alone; one not due is passed over;
    /// a non-candidate (unique work, too young, kept) is never tried.
    #[test]
    fn eviction_order_rotates_past_deferred_proofs() {
        let entries = [
            (false, None),             // unique work
            (true, Some((false, 50))), // deferred, retry not due
            (true, None),
            (true, Some((true, 30))), // deferred, due
            (true, Some((true, 10))), // deferred longer ago, due
            (true, None),
        ];
        assert_eq!(eviction_order(&entries), vec![4, 2, 5]);
        assert_eq!(
            eviction_order(&[(false, None), (false, Some((true, 1)))]),
            Vec::<usize>::new()
        );
        assert_eq!(
            eviction_order(&[(true, Some((false, 1)))]),
            Vec::<usize>::new()
        );
    }

    /// Usage serialises with both locations, so `storage status` can show
    /// bytes and counts by location.
    #[test]
    fn usage_carries_both_locations() {
        let usage = ArchiveBoundUsage {
            pool: ArchiveLocationUsage {
                archives: 2,
                bytes: 10,
                root: Some("/pool/.aethyme-recovery-archives/key".into()),
            },
            host_state: ArchiveLocationUsage {
                archives: 1,
                bytes: 5,
                root: None,
            },
            bound_bytes: 12,
            over_bound: true,
            unique_kept_over_bound: 1,
            unique_kept_sessions: vec![7],
            ..ArchiveBoundUsage::default()
        };
        let value = serde_json::to_value(&usage).unwrap();
        assert_eq!(value["pool"]["archives"], 2);
        assert_eq!(value["host_state"]["bytes"], 5);
        let back: ArchiveBoundUsage = serde_json::from_value(value).unwrap();
        assert_eq!(back, usage);
    }
}
