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
//! A pass that did not list or walk everything (`partial`) still knows the
//! total (`total_known`) when every archive is either measured by it or
//! counted with a size an earlier pass measured; within the bound, it is
//! then not due again before the normal interval. What that rests on:
//!
//! - every pass reads the names in both archive directories whole, so an
//!   archive removed elsewhere is never counted, and a new one, having no
//!   size, makes the total unknown (it is listed and walked first). A
//!   directory that cannot be listed, or a pool that was there before and is
//!   not now while sizes of archives in it are recorded, makes it unknown
//!   too, and its sizes are kept (an absent pool alone waits the interval);
//! - content the broker vouches for does not change;
//! - an archive it does not vouch for may grow: its earlier size makes the
//!   total known for a day at most, and not after a failed walk. Within that
//!   day the total may be too low by what it grew.
//!
//! The listing keeps its cursor and leaves part of the budget to the walks,
//! so successive passes reach every archive; a pass that could not move the
//! cursor does not know the total, so it stays due.
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
    /// configured one, how many of the next removal proofs start already
    /// out of budget, how many more size walks start (the rest find the
    /// budget used), whether size walks fail, and after how many
    /// removals the rest find their budget used.
    pub(super) static CLOCK_SKEW_MS_FOR_TEST: std::cell::Cell<i64> =
        const { std::cell::Cell::new(0) };
    pub(super) static BOUND_BYTES_FOR_TEST: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
    pub(super) static EXPIRED_PROOFS_FOR_TEST: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    pub(super) static WALKS_LEFT_FOR_TEST: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
    pub(super) static FAILED_WALKS_FOR_TEST: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    pub(super) static REMOVALS_LEFT_FOR_TEST: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn eviction_stopped_for_test(evicted: usize) -> bool {
    REMOVALS_LEFT_FOR_TEST.with(|stop| stop.get().is_some_and(|stop| evicted >= stop))
}

#[cfg(not(test))]
fn eviction_stopped_for_test(_evicted: usize) -> bool {
    false
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
    /// Archives listed this pass that could not be walked, counted from an
    /// earlier size (only ones the broker cannot vouch for).
    walk_fell_back: usize,
    /// Those and unlisted ones whose earlier size is too old, or whose walk
    /// failed since: counted, but the total is not known.
    size_stale: usize,
    /// Archives with no usable earlier size, listed ahead of the rotation.
    listed_first: usize,
    /// The pool seen before is absent.
    pool_absent: bool,
    /// Archives with neither a size from this pass nor an earlier one: the
    /// total is not known, so the next run measures again.
    size_unknown: usize,
    /// Archive directories whose listing failed.
    root_failures: usize,
    /// Every archive is in the total, and whether some only from an
    /// earlier size.
    total_known: bool,
    total_from_earlier_sizes: bool,
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
        if self.total_from_earlier_sizes {
            crate::phase_telemetry::add(
                "recovery_archive_bound_total_from_earlier_sizes",
                Duration::ZERO,
            );
        }
        if !self.total_known {
            crate::phase_telemetry::add("recovery_archive_bound_total_unknown", Duration::ZERO);
        }
        crate::phase_telemetry::attach(
            "recovery_archive_bound_pass",
            serde_json::json!({
                "listed": self.listed,
                "listing_skipped_by_budget": self.skipped_by_budget,
                "counted_from_earlier_size": self.counted_from_earlier_size,
                "walk_fell_back": self.walk_fell_back,
                "size_stale": self.size_stale,
                "listed_first": self.listed_first,
                "pool_absent": self.pool_absent,
                "size_unknown": self.size_unknown,
                "root_failures": self.root_failures,
                "total_known": self.total_known,
                "total_from_earlier_sizes": self.total_from_earlier_sizes,
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
    /// The budget was used before it began.
    NotStarted,
    OutOfBudget,
    Failed,
}

/// One archive to size.
enum Walk {
    Owned(crate::cleanup_resolve::OwnedRecoveryArchive),
    Unowned {
        path: PathBuf,
        /// An archive directory that could not be listed.
        root_failure: bool,
        earlier: crate::storage_ledger::UnownedArchiveSize,
    },
}

/// What [`walk_order`] needs of one archive.
enum WalkKind {
    Owned { deferred: Option<i64> },
    Unowned { tried: i64 },
    Root,
}

/// The order archives are sized in: the broker's own, unless an earlier walk
/// ran out of budget; then the ones it cannot vouch for, longest untried
/// first; then its own whose walk ran out, longest ago first, so one that
/// never fits cannot use the budget ahead of the others; then directories
/// that could not be listed.
fn walk_order(kind: WalkKind) -> (u8, i64) {
    match kind {
        WalkKind::Owned { deferred: None } => (0, 0),
        WalkKind::Unowned { tried } => (1, tried),
        WalkKind::Owned { deferred: Some(at) } => (2, at),
        WalkKind::Root => (3, 0),
    }
}

/// Whether nothing is at `path`; any other error leaves it unknown.
fn plainly_missing(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

fn walk_size(path: &Path, deadline: Instant) -> SizeWalk {
    #[cfg(test)]
    {
        let left = WALKS_LEFT_FOR_TEST.with(std::cell::Cell::get);
        if left == Some(0) {
            return SizeWalk::NotStarted;
        }
        WALKS_LEFT_FOR_TEST.with(|cell| cell.set(left.map(|left| left - 1)));
        if FAILED_WALKS_FOR_TEST.with(std::cell::Cell::get) {
            return SizeWalk::Failed;
        }
    }
    if Instant::now() >= deadline {
        return SizeWalk::NotStarted;
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
        let state_span = crate::phase_telemetry::span("recovery_archive_bound_state_read");
        let mut previous = host_state
            .as_deref()
            .map(|host| crate::storage_ledger::archive_bound_state(host, &repository))
            .unwrap_or_default();
        drop(state_span);
        // Listing and classifying read only directory entries, manifests
        // and session records: no archive content is read or hashed here.
        // The one archive about to be removed is fully proven, under the
        // lifecycle claim, in `evict_landed_archive`.
        let listing_span = crate::phase_telemetry::span("recovery_archive_bound_listing");
        let scan = match self.recovery_archive_listing(
            scan_deadline,
            previous.listing_cursor.as_deref().map(Path::new),
            // Archives with no size to count, and those whose size is too
            // old to rely on, are reached first.
            &|path: &Path| {
                let path = path.to_string_lossy();
                previous.unowned_sizes.get(&*path).map_or_else(
                    || previous.sizes.contains_key(&*path),
                    |unowned| unowned.fresh(pass_now_ms()),
                )
            },
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
            root_failures: scan.root_failures,
            listed_first: scan.listed_first,
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
        let pool_root = scan.pool_root.clone();
        // A pool seen before and absent now (unmounted, or its marker does
        // not match) may hold archives: nothing under it was listed. One in
        // which nothing is recorded is forgotten.
        let absent_pool = previous
            .pool_root
            .as_ref()
            .filter(|_| pool_root.is_none())
            .filter(|root| {
                previous
                    .sizes
                    .keys()
                    .chain(previous.unowned_sizes.keys())
                    .any(|path| Path::new(path).starts_with(root))
            })
            .map(PathBuf::from);
        state.pool_root = pool_root
            .as_ref()
            .map(|root| std::fs::canonicalize(root).unwrap_or_else(|_| root.clone()))
            .or_else(|| absent_pool.clone())
            .map(|root| root.to_string_lossy().into_owned());
        stats.pool_absent = absent_pool.is_some();
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
        //
        // Archives the broker cannot vouch for are walked afresh whenever
        // reached (their content may change), the one tried longest ago
        // first, and are never removed. One whose walk does not complete
        // counts in the total with its last measured size, if any. An
        // archive directory that could not be listed is among them: what is
        // in it is unknown, and it keeps no size of its own.
        let sizing_span = crate::phase_telemetry::span("recovery_archive_bound_sizing");
        let size_now = pass_now_ms();
        let mut walks = scan
            .owned
            .into_iter()
            .map(|archive| {
                let deferred = previous
                    .size_deferred_at_ms
                    .get(&key(&archive.path))
                    .copied();
                (
                    walk_order(WalkKind::Owned { deferred }),
                    Walk::Owned(archive),
                )
            })
            .chain(
                scan.unowned
                    .into_iter()
                    .enumerate()
                    .map(|(index, (path, _))| {
                        let root_failure = index < scan.root_failures;
                        let earlier = previous
                            .unowned_sizes
                            .get(&key(&path))
                            .copied()
                            .unwrap_or_default();
                        let kind = if root_failure {
                            WalkKind::Root
                        } else {
                            WalkKind::Unowned {
                                tried: earlier.tried_at_ms,
                            }
                        };
                        (
                            walk_order(kind),
                            Walk::Unowned {
                                path,
                                root_failure,
                                earlier,
                            },
                        )
                    }),
            )
            .collect::<Vec<_>>();
        walks.sort_by_key(|(order, _)| *order);
        // Bytes of archives the broker cannot vouch for that this pass did
        // not walk. They count in the total and against the bound, but never
        // towards removing an archive: that size may be out of date.
        let mut unwalked_unowned_bytes = 0_u64;
        let mut sized = Vec::new();
        for (_, walk) in walks {
            match walk {
                Walk::Owned(archive) => {
                    let path_key = key(&archive.path);
                    previous.unowned_sizes.remove(&path_key);
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
                    // A size recorded for another archive at this path is
                    // not this one's: a later pass must not count it.
                    previous.sizes.remove(&path_key);
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
                            continue;
                        }
                        SizeWalk::NotStarted => stats.size_skipped_by_budget += 1,
                        SizeWalk::OutOfBudget => {
                            // Only one that was tried: the others go first
                            // next time.
                            stats.size_skipped_by_budget += 1;
                            state.size_deferred_at_ms.insert(path_key, size_now);
                        }
                        SizeWalk::Failed => stats.size_failed += 1,
                    }
                    stats.size_unknown += 1;
                    usage.partial = true;
                }
                Walk::Unowned {
                    path,
                    root_failure,
                    earlier,
                } => {
                    let mut record = earlier;
                    let walked = walk_size(&path, scan_deadline);
                    if !matches!(walked, SizeWalk::NotStarted) {
                        record.tried_at_ms = size_now;
                    }
                    let complete = matches!(walked, SizeWalk::Measured(_));
                    match walked {
                        SizeWalk::Measured(bytes) => {
                            stats.size_measured += 1;
                            record.bytes = Some(bytes);
                            record.measured_at_ms = size_now;
                        }
                        SizeWalk::NotStarted | SizeWalk::OutOfBudget => {
                            stats.size_skipped_by_budget += 1;
                            usage.partial = true;
                        }
                        SizeWalk::Failed => {
                            stats.size_failed += 1;
                            usage.partial = true;
                            if !root_failure && plainly_missing(&path) {
                                // Removed since it was listed.
                                previous.unowned_sizes.remove(&key(&path));
                                previous.sizes.remove(&key(&path));
                                continue;
                            }
                            record.measured_at_ms = 0;
                        }
                    }
                    if let Some(bytes) = record.bytes {
                        let location = if in_pool(&path) {
                            &mut usage.pool
                        } else {
                            &mut usage.host_state
                        };
                        location.archives += 1;
                        location.bytes = location.bytes.saturating_add(bytes);
                        if !complete {
                            stats.walk_fell_back += 1;
                            unwalked_unowned_bytes = unwalked_unowned_bytes.saturating_add(bytes);
                            if !record.fresh(size_now) {
                                stats.size_stale += 1;
                            }
                        }
                    } else if !root_failure {
                        stats.size_unknown += 1;
                    }
                    if !root_failure {
                        state.unowned_sizes.insert(key(&path), record);
                    }
                }
            }
        }
        // Archives this listing did not reach count with the size an
        // earlier pass measured, so a long listing does not hide them from
        // the total; they are not candidates this pass. A size walked while
        // the broker could not vouch for the archive is the more recent one
        // (it is forgotten once the archive is listed as the broker's).
        for path in &scan.not_listed {
            let path_key = key(path);
            let mut earlier = None;
            if let Some(known) = previous.sizes.get(&path_key) {
                state.sizes.insert(path_key.clone(), *known);
                earlier = Some(known.bytes);
            }
            if let Some(unowned) = previous.unowned_sizes.get(&path_key) {
                state.unowned_sizes.insert(path_key, *unowned);
                earlier = unowned.bytes;
                if let Some(bytes) = unowned.bytes {
                    unwalked_unowned_bytes = unwalked_unowned_bytes.saturating_add(bytes);
                    if !unowned.fresh(size_now) {
                        stats.size_stale += 1;
                    }
                }
            }
            let Some(bytes) = earlier else {
                stats.size_unknown += 1;
                continue;
            };
            stats.counted_from_earlier_size += 1;
            let location = if in_pool(path) {
                &mut usage.pool
            } else {
                &mut usage.host_state
            };
            location.archives += 1;
            location.bytes = location.bytes.saturating_add(bytes);
        }
        // A pass whose rotation listed nothing while archives wait made no
        // progress: it stays due, so waiting an interval always moves the
        // cursor.
        let stalled = scan.last_listed.is_none() && !scan.not_listed.is_empty();
        let reachable_known =
            stats.size_unknown == 0 && stats.size_stale == 0 && scan.root_failures == 0 && !stalled;
        usage.total_known = reachable_known && !stats.pool_absent;
        // Measuring again at once cannot bring an absent pool back: alone,
        // it waits for the interval.
        usage.only_pool_absent = reachable_known && stats.pool_absent;
        stats.total_known = usage.total_known;
        stats.total_from_earlier_sizes =
            usage.total_known && stats.counted_from_earlier_size + stats.walk_fell_back > 0;
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
        for (position, &index) in order.iter().enumerate() {
            if Instant::now() >= evict_deadline || eviction_stopped_for_test(usage.evicted) {
                stats.budget_stops += 1;
                // Only a pass that removed something: aged archives that
                // never land would otherwise keep it due for ever.
                usage.eviction_unfinished = usage.evicted > 0
                    && order[position..].iter().any(|&left| {
                        measured[left].aged_out && measured[left].landed != Some(false)
                    });
                break;
            }
            let needed = measured[index].aged_out
                || (usage.bound_bytes > 0
                    && total.saturating_sub(unwalked_unowned_bytes) > usage.bound_bytes);
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
        // reach, while they are still there. A size this pass already holds
        // (listed now, or counted from the earlier size) needs no `lstat`:
        // keeping it changes nothing, and asking for each of 2,373 archives
        // took about 70 s of one pass on the busiest repository.
        let carry_span = crate::phase_telemetry::span("recovery_archive_bound_state_carry");
        // Only a path plainly missing is dropped, and none of an absent pool.
        let still_there = |path: &str| {
            absent_pool
                .as_ref()
                .is_some_and(|root| Path::new(path).starts_with(root))
                || !plainly_missing(Path::new(path))
        };
        for (path, known) in previous.sizes {
            if !state.sizes.contains_key(&path) && still_there(&path) {
                state.sizes.insert(path, known);
            }
        }
        for (path, known) in previous.unowned_sizes {
            if !state.unowned_sizes.contains_key(&path) && still_there(&path) {
                state.unowned_sizes.insert(path, known);
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
        drop(carry_span);
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
                "total_known": usage.total_known,
                "only_pool_absent": usage.only_pool_absent,
                "eviction_unfinished": usage.eviction_unfinished,
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

    /// This repository's last recorded bound pass; `None` when none is
    /// recorded.
    pub(crate) fn last_recovery_archive_bound(&self) -> Option<LastArchiveBound> {
        let (host_state, _) = crate::host_state::resolve_host_state_dir()?;
        let repository = self.main_root().to_string_lossy().into_owned();
        crate::storage_ledger::recovery_archive_usage(&host_state)
            .get(&repository)
            .map(|usage| LastArchiveBound {
                observed_at_ms: usage.observed_at_ms,
                over_bound: usage.over_bound,
                partial: usage.partial,
                total_known: usage.total_known,
                only_pool_absent: usage.only_pool_absent,
                eviction_unfinished: usage.eviction_unfinished,
            })
    }
}

/// What the last recorded bound pass found, for deciding whether the next
/// one is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LastArchiveBound {
    /// When the pass started (epoch ms).
    pub(crate) observed_at_ms: i64,
    /// The archives were over the bound.
    pub(crate) over_bound: bool,
    /// The pass did not list and walk every archive (a budget stop or a
    /// failed listing).
    pub(crate) partial: bool,
    /// Every archive is in its total all the same, some from a size an
    /// earlier pass measured; otherwise it may undercount.
    pub(crate) total_known: bool,
    /// The total is not known for an absent pool alone.
    pub(crate) only_pool_absent: bool,
    /// Removals made some and stopped on their budget with aged-out
    /// archives left.
    pub(crate) eviction_unfinished: bool,
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

    /// Own archives never deferred, then unowned ones longest untried first,
    /// then own ones deferred longest ago, then unlistable directories.
    #[test]
    fn walks_are_ordered_so_one_large_archive_cannot_starve_the_rest() {
        let order = [
            walk_order(WalkKind::Owned { deferred: None }),
            walk_order(WalkKind::Unowned { tried: 0 }),
            walk_order(WalkKind::Unowned { tried: 9 }),
            walk_order(WalkKind::Owned { deferred: Some(1) }),
            walk_order(WalkKind::Owned { deferred: Some(2) }),
            walk_order(WalkKind::Root),
        ];
        assert!(order.is_sorted(), "{order:?}");
    }

    /// Records from before `total_known` and the sizes of unowned archives
    /// read as a total not known and no such size.
    #[test]
    fn records_from_before_known_totals_still_read() {
        let mut usage = serde_json::to_value(ArchiveBoundUsage {
            total_known: true,
            ..ArchiveBoundUsage::default()
        })
        .unwrap();
        usage.as_object_mut().unwrap().remove("total_known");
        let usage: ArchiveBoundUsage = serde_json::from_value(usage).unwrap();
        assert!(!usage.total_known);
        let state: ArchiveBoundState = serde_json::from_value(serde_json::json!({
            "sizes": {"/a/1-x": {"created_at_ms": 5, "bytes": 7}},
        }))
        .unwrap();
        assert_eq!(state.sizes["/a/1-x"].bytes, 7);
        assert!(state.unowned_sizes.is_empty());
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
