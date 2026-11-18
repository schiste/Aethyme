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
//! What it measured is recorded per repository in the storage ledger
//! (`storage-ledger-recovery-archives/<sha256 of the repository root>.json`),
//! with bytes and counts by location, and shown by `aethyme storage status`.

use super::*;
use std::time::{Duration, Instant};

use crate::storage_ledger::{ArchiveLocationUsage, RecoveryArchiveUsage as ArchiveBoundUsage};

/// The longest one bound pass may take.
const ARCHIVE_BOUND_BUDGET: Duration = Duration::from_secs(20);
/// Sessions named one by one in the advice, at most.
const UNIQUE_SESSIONS_NAMED: usize = 20;

struct Measured {
    archive: crate::cleanup_resolve::OwnedRecoveryArchive,
    bytes: u64,
    evictable: bool,
    /// Fully landed and older than `recovery_archive_days`.
    expired: bool,
}

impl Broker {
    /// Measure this repository's recovery archives against the bound and
    /// remove the oldest fully landed ones while over it (see the module
    /// documentation). Never removes an archive holding unique work.
    pub(crate) fn enforce_recovery_archive_bound(&self, deadline: Instant) -> ArchiveBoundUsage {
        let _span = crate::phase_telemetry::span("recovery_archive_bound_pass");
        let deadline = deadline.min(Instant::now() + ARCHIVE_BOUND_BUDGET);
        let mut usage = ArchiveBoundUsage {
            bound_bytes: crate::auto_cleanup::recovery_archive_bound_bytes(self.main_root()),
            observed_at_ms: crate::clock::epoch_ms(),
            ..ArchiveBoundUsage::default()
        };
        let scan = match self.recovery_archives() {
            Ok(scan) => scan,
            Err(error) => {
                usage.partial = true;
                crate::warn_unrecorded::<(), _>("scan recovery archives for the bound", Err(error));
                return usage;
            }
        };
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
        let now = crate::clock::epoch_ms();
        let retention_days = crate::retention::load_retention_policy(self.main_root())
            .map_or(90, |policy| policy.recovery_archive_days);
        let mut measured = Vec::new();
        for archive in scan.owned {
            if Instant::now() >= deadline {
                usage.partial = true;
                break;
            }
            let bytes =
                super::status_helpers::directory_size_without_following_links(&archive.path)
                    .unwrap_or(0);
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
            let evictable = old_enough && self.recovery_archive_landed(&archive);
            // Host state archives keep expiring through the storage worker
            // (with its notice week); pool ones are this lane's.
            let expired = evictable
                && in_pool(&archive.path)
                && retention_days > 0
                && now.saturating_sub(archive.created_at_ms)
                    >= i64::from(retention_days).saturating_mul(86_400_000);
            measured.push(Measured {
                archive,
                bytes,
                evictable,
                expired,
            });
        }
        // Archives the broker cannot vouch for count, and are never removed.
        for (path, _) in &scan.unowned {
            let bytes =
                super::status_helpers::directory_size_without_following_links(path).unwrap_or(0);
            let location = if in_pool(path) {
                &mut usage.pool
            } else {
                &mut usage.host_state
            };
            location.archives += 1;
            location.bytes = location.bytes.saturating_add(bytes);
        }
        let mut total = usage.pool.bytes.saturating_add(usage.host_state.bytes);
        // Fully landed and past their retention age: removed as before.
        for entry in measured.iter().filter(|entry| entry.expired) {
            if Instant::now() >= deadline {
                break;
            }
            if self.evict_landed_archive(&entry.archive, deadline).is_ok() {
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
                crate::phase_telemetry::add("recovery_archive_bound_expired", Duration::ZERO);
            }
        }
        measured.retain(|entry| std::fs::symlink_metadata(&entry.archive.path).is_ok());
        if usage.bound_bytes == 0 || total <= usage.bound_bytes {
            crate::phase_telemetry::add("recovery_archive_bound_within", Duration::ZERO);
            return usage;
        }
        measured.sort_by_key(|entry| (entry.archive.created_at_ms, entry.archive.session_id));
        let order = plan_evictions(
            &measured
                .iter()
                .map(|entry| (entry.bytes, entry.evictable))
                .collect::<Vec<_>>(),
            total,
            usage.bound_bytes,
        );
        for entry in order.into_iter().map(|index| &measured[index]) {
            if total <= usage.bound_bytes || Instant::now() >= deadline {
                break;
            }
            match self.evict_landed_archive(&entry.archive, deadline) {
                Ok(()) => {
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
                    crate::phase_telemetry::add("recovery_archive_bound_evicted", Duration::ZERO);
                }
                Err(reason) => {
                    crate::phase_telemetry::add(
                        "recovery_archive_bound_evict_failed",
                        Duration::ZERO,
                    );
                    crate::warn_unrecorded::<(), _>(
                        "remove a landed recovery archive over the bound",
                        Err(reason),
                    );
                }
            }
        }
        usage.over_bound = total > usage.bound_bytes;
        if usage.over_bound {
            let unique = measured
                .iter()
                .filter(|entry| !self.recovery_archive_landed(&entry.archive))
                .filter(|entry| std::fs::symlink_metadata(&entry.archive.path).is_ok())
                .collect::<Vec<_>>();
            usage.unique_kept_over_bound = unique.len();
            usage.unique_kept_sessions = unique
                .iter()
                .map(|entry| entry.archive.session_id)
                .take(UNIQUE_SESSIONS_NAMED)
                .collect();
            crate::phase_telemetry::add("recovery_archive_bound_over", Duration::ZERO);
        }
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
    fn evict_landed_archive(
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
        let current = self.owned_recovery_archive(&archive.path)?;
        if current.session_id != archive.session_id
            || current.head != archive.head
            || !self.recovery_archive_landed(&current)
        {
            return Err("no longer a fully landed archive".into());
        }
        match crate::gc::remove_condemned_tree(
            &current.path,
            Some(crate::cleanup_resolve::MANIFEST_FILE),
            Some(deadline),
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

/// Which archives to remove, in order: from `entries` (bytes, evictable)
/// sorted oldest first, the evictable ones, oldest first, until `total`
/// would be within `bound`. A non-evictable entry (work that exists nowhere
/// else, or younger than its minimum retention) is never chosen.
fn plan_evictions(entries: &[(u64, bool)], total: u64, bound: u64) -> Vec<usize> {
    let mut remaining = total;
    let mut chosen = Vec::new();
    for (index, (bytes, evictable)) in entries.iter().enumerate() {
        if remaining <= bound {
            break;
        }
        if *evictable {
            chosen.push(index);
            remaining = remaining.saturating_sub(*bytes);
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Over the bound, the oldest evictable archives go first and only
    /// until the total is within it; an archive holding unique work is
    /// never chosen, even when that leaves the total over the bound.
    #[test]
    fn eviction_takes_the_oldest_landed_archives_and_never_unique_work() {
        // Oldest first: unique 40, landed 10, landed 10, landed 10.
        let entries = [(40, false), (10, true), (10, true), (10, true)];
        assert_eq!(plan_evictions(&entries, 70, 55), vec![1, 2]);
        assert_eq!(plan_evictions(&entries, 70, 70), Vec::<usize>::new());
        // Only unique work: nothing is removed whatever the bound.
        assert!(plan_evictions(&[(40, false), (30, false)], 70, 0).is_empty());
        // Everything landed still leaves unique work alone.
        assert_eq!(plan_evictions(&entries, 70, 0), vec![1, 2, 3]);
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
