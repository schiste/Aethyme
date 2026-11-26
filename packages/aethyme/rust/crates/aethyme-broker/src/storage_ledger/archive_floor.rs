//! The recovery archive floor: the free space a volume holding recovery
//! archives keeps, whatever each repository's own archive bound allows.
//!
//! Each repository bounds its own archives (`[cleanup]
//! recovery_archive_bound_gib`), and the host bounds the archives in host
//! state together (`[budget] recovery_archives_bytes`); neither looks at the
//! volume. With several repositories, archives within every bound can still
//! fill the disk host state lives on. The floor is the larger of
//! `[budget] free_floor_bytes` and `recovery_archive_free_floor_percent` of
//! the volume's size, the share capped at
//! `recovery_archive_free_floor_cap_bytes`: with the defaults (10 GiB, 10%,
//! 20 GiB) a 1 TB disk (931 GiB) gets 20 GiB, not 93 GiB, and a 100 GB disk
//! 10 GiB. While the volume's free space is below it, the worker
//! removes the oldest removable archives on that volume, whichever
//! repository they belong to, until the free space it projects (the
//! reading plus what it removed) is back at the floor.
//!
//! An archive the other rules keep is never removed for the floor: kept by
//! its owner's marker, within its promised minimum retention, without a
//! creation time, still being written, or not proven landed (commits not in
//! the default branch, or any patch, untracked or ignored file), the last
//! proven only when the archive is reached. Removals are paced like every
//! unattended deletion, even under pressure, and stop at a time budget
//! shared by every volume in the pass.
//!
//! A floor the archives cannot meet is made visible: when the bytes every
//! archive not already kept by a rule holds (an upper bound, landing not yet
//! proven) could not close a quarter of the shortfall, the archives that can
//! go still go, and the stop is `cannot_close` rather than `protected_only`
//! (on APFS, free space excludes purgeable space, so a floor can be out of
//! reach of anything Aethyme removes).
//!
//! This module holds the selection rules; `check::run_pass` applies them.

use serde::{Deserialize, Serialize};

/// Why the floor stopped removing archives on a volume.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FloorStop {
    /// Free space is at or above the floor (it may have been all along).
    #[default]
    FloorMet,
    /// Below the floor, but every archive left is one a rule keeps.
    ProtectedOnly,
    /// Below the floor, and even every archive no rule kept in advance
    /// could not have closed a quarter of the shortfall.
    CannotClose,
    /// Below the floor when the time budget ran out (or the worker lock
    /// was lost); the next pass continues.
    Deadline,
    /// The volume's free space cannot be read: the floor is skipped.
    Unsupported,
}

impl FloorStop {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FloorMet => "floor_met",
            Self::ProtectedOnly => "protected_only",
            Self::CannotClose => "cannot_close",
            Self::Deadline => "deadline",
            Self::Unsupported => "unsupported",
        }
    }

    /// The counter one stop adds.
    pub fn counter(self) -> &'static str {
        match self {
            Self::FloorMet => "storage_archive_floor_stop_floor_met",
            Self::ProtectedOnly => "storage_archive_floor_stop_protected_only",
            Self::CannotClose => "storage_archive_floor_stop_cannot_close",
            Self::Deadline => "storage_archive_floor_stop_deadline",
            Self::Unsupported => "storage_archive_floor_stop_unsupported",
        }
    }
}

/// One archive on the volume, as the floor sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloorArchive {
    /// The manifest's creation time (epoch ms).
    pub created_ms: u64,
    /// Its measured size.
    pub bytes: u64,
    /// A rule known before any Git read keeps it: the owner's keep marker,
    /// its promised minimum retention, no creation time, or still being
    /// written.
    pub protected: bool,
}

/// Progress towards the floor on one volume: the free space read before
/// any removal (`None` when it cannot be read) plus the bytes removed since,
/// and the bytes of the archives no rule kept in advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloorProgress {
    free_before: Option<u64>,
    floor: u64,
    freed: u64,
    removable: u64,
}

impl FloorProgress {
    pub fn new(free_before: Option<u64>, floor: u64) -> Self {
        Self {
            free_before,
            floor,
            freed: 0,
            removable: 0,
        }
    }

    /// The progress on `archives`, counting what they could free at most.
    pub fn for_archives(archives: &[FloorArchive], free_before: Option<u64>, floor: u64) -> Self {
        Self {
            removable: archives
                .iter()
                .filter(|archive| !archive.protected)
                .fold(0, |sum: u64, archive| sum.saturating_add(archive.bytes)),
            ..Self::new(free_before, floor)
        }
    }

    /// Whether every archive no rule keeps in advance could not close a
    /// quarter of the shortfall the volume started with.
    pub fn cannot_close(&self) -> bool {
        self.free_before.is_some_and(|free| {
            let shortfall = self.floor.saturating_sub(free);
            shortfall > 0 && self.removable.saturating_mul(4) < shortfall
        })
    }

    /// Whether the projected free space is still below the floor. An
    /// unreadable volume is never short: nothing is removed on a guess.
    pub fn short(&self) -> bool {
        self.free_before
            .is_some_and(|free| free.saturating_add(self.freed) < self.floor)
    }

    /// Count one removal of `bytes`.
    pub fn record(&mut self, bytes: u64) {
        self.freed = self.freed.saturating_add(bytes);
    }

    /// Why removals stopped, given whether the time budget ran out.
    pub fn stop(&self, out_of_time: bool) -> FloorStop {
        if self.free_before.is_none() {
            FloorStop::Unsupported
        } else if !self.short() {
            FloorStop::FloorMet
        } else if self.cannot_close() {
            FloorStop::CannotClose
        } else if out_of_time {
            FloorStop::Deadline
        } else {
            FloorStop::ProtectedOnly
        }
    }
}

/// The order removals are tried in: the unprotected archives, oldest first
/// by creation (ties by position), whichever repository each belongs to.
pub fn floor_order(archives: &[FloorArchive]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..archives.len())
        .filter(|&index| !archives[index].protected)
        .collect();
    order.sort_by_key(|&index| (archives[index].created_ms, index));
    order
}

/// The archives the floor removes, assuming each removal frees its measured
/// size, and why it stops: oldest first, protected ones never, and no more
/// once the projected free space reaches the floor.
pub fn select(archives: &[FloorArchive], free: Option<u64>, floor: u64) -> (Vec<usize>, FloorStop) {
    let mut progress = FloorProgress::for_archives(archives, free, floor);
    let mut chosen = Vec::new();
    for index in floor_order(archives) {
        if !progress.short() {
            break;
        }
        chosen.push(index);
        progress.record(archives[index].bytes);
    }
    (chosen, progress.stop(false))
}

/// What the floor did on one volume, in the pass report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloorReport {
    /// The volume (device number).
    pub device: u64,
    pub floor_bytes: u64,
    /// Free space before and after the removals; `None` when unreadable.
    pub free_before_bytes: Option<u64>,
    pub free_after_bytes: Option<u64>,
    /// Archives removed for the floor, and their measured bytes (complete
    /// removals only).
    pub pruned: usize,
    pub pruned_bytes: u64,
    /// Removals the deadline interrupted after the rename: gone from their
    /// place, the remainder swept by the next pass; not counted as freed.
    #[serde(default)]
    pub interrupted: usize,
    pub stop: FloorStop,
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn archive(created_ms: u64, bytes: u64) -> FloorArchive {
        FloorArchive {
            created_ms,
            bytes,
            protected: false,
        }
    }

    #[test]
    fn nothing_goes_while_the_floor_is_met() {
        let archives = [archive(1, GIB), archive(2, GIB)];
        assert_eq!(
            select(&archives, Some(10 * GIB), 10 * GIB),
            (vec![], FloorStop::FloorMet)
        );
        assert_eq!(
            select(&archives, Some(20 * GIB), 10 * GIB),
            (vec![], FloorStop::FloorMet)
        );
    }

    #[test]
    fn protected_archives_are_never_chosen() {
        let archives = [
            FloorArchive {
                protected: true,
                ..archive(1, 5 * GIB)
            },
            archive(2, GIB),
            FloorArchive {
                protected: true,
                ..archive(3, 5 * GIB)
            },
        ];
        // 8 free of 10: the one unprotected archive could close the 2 GiB
        // shortfall's quarter, but not all of it.
        assert_eq!(
            select(&archives, Some(8 * GIB), 10 * GIB),
            (vec![1], FloorStop::ProtectedOnly)
        );
        let all_protected = [FloorArchive {
            protected: true,
            ..archive(1, 50 * GIB)
        }];
        assert_eq!(
            select(&all_protected, Some(GIB), 10 * GIB),
            (vec![], FloorStop::CannotClose)
        );
    }

    /// Archives of several repositories, listed repository by repository,
    /// go strictly by age.
    #[test]
    fn oldest_go_first_across_repositories() {
        let archives = [
            archive(30, GIB), // first repository
            archive(10, GIB),
            archive(20, GIB), // second repository
            archive(5, GIB),
        ];
        assert_eq!(floor_order(&archives), vec![3, 1, 2, 0]);
        assert_eq!(
            select(&archives, Some(7 * GIB), 10 * GIB),
            (vec![3, 1, 2], FloorStop::FloorMet)
        );
    }

    #[test]
    fn stops_as_soon_as_projected_free_space_reaches_the_floor() {
        let archives = [archive(1, 3 * GIB), archive(2, 4 * GIB), archive(3, GIB)];
        // 4 + 3 = 7 short of 10; + 4 = 11: two removals, not three.
        assert_eq!(
            select(&archives, Some(4 * GIB), 10 * GIB),
            (vec![0, 1], FloorStop::FloorMet)
        );
        // Exactly at the floor after the first.
        assert_eq!(
            select(&archives, Some(7 * GIB), 10 * GIB),
            (vec![0], FloorStop::FloorMet)
        );
    }

    #[test]
    fn an_unreadable_volume_skips_the_floor() {
        let archives = [archive(1, GIB)];
        assert_eq!(
            select(&archives, None, 10 * GIB),
            (vec![], FloorStop::Unsupported)
        );
        assert!(!FloorProgress::new(None, u64::MAX).short());
    }

    #[test]
    fn a_stop_on_time_is_a_deadline_only_while_short() {
        let mut progress = FloorProgress::for_archives(&[archive(1, GIB)], Some(GIB), 2 * GIB);
        assert_eq!(progress.stop(true), FloorStop::Deadline);
        assert_eq!(progress.stop(false), FloorStop::ProtectedOnly);
        progress.record(GIB);
        assert_eq!(progress.stop(true), FloorStop::FloorMet);
    }

    /// Even every unprotected archive could not close a quarter of the
    /// shortfall: they still go, and the stop says the floor is out of reach.
    #[test]
    fn a_floor_out_of_reach_is_reported_as_cannot_close() {
        let archives = [
            archive(1, GIB),
            FloorArchive {
                protected: true,
                ..archive(2, 50 * GIB)
            },
        ];
        // Shortfall 10 GiB; 1 GiB removable is under a quarter.
        assert_eq!(
            select(&archives, Some(0), 10 * GIB),
            (vec![0], FloorStop::CannotClose)
        );
        // 3 GiB removable closes over a quarter: protected_only.
        let archives = [archive(1, 3 * GIB)];
        assert_eq!(
            select(&archives, Some(0), 10 * GIB),
            (vec![0], FloorStop::ProtectedOnly)
        );
        let progress = FloorProgress::for_archives(&archives, Some(0), 10 * GIB);
        assert!(!progress.cannot_close());
        assert_eq!(progress.stop(true), FloorStop::Deadline);
        let hopeless = FloorProgress::for_archives(&archives, Some(0), 100 * GIB);
        assert_eq!(hopeless.stop(true), FloorStop::CannotClose);
    }

    #[test]
    fn the_floor_is_the_larger_of_the_fixed_floor_and_the_share() {
        let budgets = super::super::Budgets::default();
        assert_eq!(budgets.recovery_archive_floor(0), 10 * GIB);
        assert_eq!(budgets.recovery_archive_floor(50 * GIB), 10 * GIB);
        assert_eq!(budgets.recovery_archive_floor(150 * GIB), 15 * GIB);
        // The share is capped: 20 GiB for a 1 TB disk, not 93 GiB.
        assert_eq!(budgets.recovery_archive_floor(1_000_000_000_000), 20 * GIB);
        assert_eq!(budgets.recovery_archive_floor(500 * GIB), 20 * GIB);
        let uncapped = super::super::Budgets {
            recovery_archive_free_floor_cap_bytes: u64::MAX,
            ..budgets.clone()
        };
        assert_eq!(uncapped.recovery_archive_floor(500 * GIB), 50 * GIB);
        let fixed_only = super::super::Budgets {
            recovery_archive_free_floor_percent: 0,
            ..budgets.clone()
        };
        assert_eq!(fixed_only.recovery_archive_floor(500 * GIB), 10 * GIB);
        let off = super::super::Budgets {
            recovery_archive_free_floor_percent: 0,
            free_floor_bytes: 0,
            ..budgets
        };
        assert_eq!(off.recovery_archive_floor(500 * GIB), 0);
        assert!(!FloorProgress::new(Some(0), 0).short());
        assert_eq!(uncapped.recovery_archive_floor(u64::MAX), u64::MAX / 10);
    }
}
