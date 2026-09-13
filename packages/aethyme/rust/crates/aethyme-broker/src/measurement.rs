//! What the broker knows about how big things are, and what it may claim when
//! it has not looked.
//!
//! Sizing a directory has no shortcut. The filesystem stores no subtree total,
//! so every byte figure in a GC plan costs a full recursive walk, and on the
//! machine that motivated #176 that walk covered ~96 GB and took `gc plan
//! --json` over five minutes. The problem was not that the walk was slow. It
//! was that `broker status` -- the mandated first step of every session --
//! reached the same walk through the same `cleanup_plan`, so the routine check
//! and the expensive audit were one code path wearing two names.
//!
//! They are split here by a single question: who may measure. [`SizeScan::Measure`]
//! walks the trees and writes down what it found. [`SizeScan::Recorded`] reads
//! those records and never touches a tree, apart from one bounded measurement
//! per pass so that a machine nobody ever runs `gc plan` on still learns its
//! own size instead of waiting for someone to ask.
//!
//! A total assembled from records is a **floor**: it omits whatever has never
//! been measured. A floor answers some questions and not others, and the
//! difference is the whole point of [`BudgetVerdict`] -- reporting a floor as
//! if it were a measurement is how "3.2 MB reclaimable" came to sit next to
//! "37.4 GB retained" with nobody noticing which of the two had been checked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::BrokerOpError;

pub const SIZE_RECORD_SCHEMA_VERSION: u32 = 1;

/// Where measurements are written down, relative to the main checkout.
const SIZE_RECORDS_RELPATH: &str = ".aethyme/worktree-sizes.json";

/// Whether a pass may walk the disk to find out how big something is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeScan {
    /// Measure nothing. Sizes come from what an earlier walk wrote down, and a
    /// directory nobody has measured stays *unmeasured* rather than silently
    /// becoming a zero.
    Recorded,
    /// Walk every tree and record what the walk found. This is the expensive
    /// path, and the only one whose totals are complete.
    Measure,
}

impl SizeScan {
    pub fn measures(self) -> bool {
        matches!(self, Self::Measure)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Measure => "measure",
        }
    }
}

/// One directory, as large as it was when somebody last walked it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SizeRecord {
    pub bytes: u64,
    pub measured_at_ms: i64,
}

/// Every measurement the broker has kept.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SizeRecords {
    pub schema_version: u32,
    pub records: BTreeMap<String, SizeRecord>,
}

impl Default for SizeRecords {
    fn default() -> Self {
        Self {
            schema_version: SIZE_RECORD_SCHEMA_VERSION,
            records: BTreeMap::new(),
        }
    }
}

impl SizeRecords {
    pub fn get(&self, path: &str) -> Option<SizeRecord> {
        self.records.get(path).copied()
    }

    pub fn record(&mut self, path: &str, bytes: u64, measured_at_ms: i64) {
        self.records.insert(
            path.to_string(),
            SizeRecord {
                bytes,
                measured_at_ms,
            },
        );
    }

    /// Forget measurements for paths the broker no longer tracks, returning
    /// how many went.
    ///
    /// Only a pass that enumerated everything may call this: a routine check
    /// sees whichever subset it was asked about, and pruning against that
    /// would throw away exactly the records it could not afford to remeasure.
    pub fn retain_paths(&mut self, live: &BTreeSet<String>) -> usize {
        let before = self.records.len();
        self.records.retain(|path, _| live.contains(path));
        before.saturating_sub(self.records.len())
    }

    /// The one path a routine pass should spend its measurement budget on.
    ///
    /// Never-measured paths come first, because a missing record is what makes
    /// a total a floor and closing that gap is worth more than refreshing a
    /// figure that is merely old. After those, the oldest record past `ttl_ms`.
    /// `None` when every path carries a record younger than the TTL, which is
    /// the steady state: warming stops on its own once it is done.
    ///
    /// Ties resolve to the earlier entry in `paths`, so a caller passing them
    /// in a stable order gets a stable rotation rather than measuring the same
    /// directory forever.
    pub fn next_to_measure(&self, paths: &[String], now_ms: i64, ttl_ms: i64) -> Option<String> {
        let mut due: Option<(i64, &String)> = None;
        for path in paths {
            let measured_at = match self.records.get(path) {
                // Never measured. Ranked ahead of every real timestamp.
                None => i64::MIN,
                Some(record) if now_ms.saturating_sub(record.measured_at_ms) >= ttl_ms => {
                    record.measured_at_ms
                }
                Some(_) => continue,
            };
            if due.is_none_or(|(best, _)| measured_at < best) {
                due = Some((measured_at, path));
            }
        }
        due.map(|(_, path)| path.clone())
    }
}

/// A byte total together with what it leaves out.
///
/// The counts are not decoration. A total with `unmeasured > 0` is a floor:
/// the real figure is this or larger. Reading `bytes` without reading
/// `unmeasured` treats a floor and a measurement as the same kind of number,
/// and they are not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MeasuredTotal {
    pub bytes: u64,
    pub measured: usize,
    pub unmeasured: usize,
    /// The oldest measurement that went into `bytes`, so a reader can tell a
    /// fresh total from one assembled out of month-old records.
    pub oldest_measured_at_ms: Option<i64>,
}

impl MeasuredTotal {
    pub fn complete(&self) -> bool {
        self.unmeasured == 0
    }

    pub fn add_measured(&mut self, bytes: u64, measured_at_ms: i64) {
        self.bytes = self.bytes.saturating_add(bytes);
        self.measured += 1;
        self.oldest_measured_at_ms = Some(match self.oldest_measured_at_ms {
            Some(oldest) => oldest.min(measured_at_ms),
            None => measured_at_ms,
        });
    }

    /// Count a directory whose size nobody knows. Deliberately adds no bytes:
    /// guessing here is what turns a floor into a wrong measurement.
    pub fn add_unmeasured(&mut self) {
        self.unmeasured += 1;
    }
}

/// What a byte total can conclude about `retained_bytes_budget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetVerdict {
    /// No budget is configured, so there is nothing to be over.
    Unset,
    /// Measured in full, and under the budget.
    Within,
    /// At or over the budget. Conclusive even from a floor: bytes nobody has
    /// counted yet can only push a total further over.
    Over,
    /// Under the budget, but only because part of the total was never
    /// measured. Reporting this as [`Within`](Self::Within) is how a budget
    /// stops meaning anything -- the gap that decides the answer is exactly
    /// the part nobody looked at.
    Unknown,
}

impl BudgetVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::Within => "within",
            Self::Over => "over",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the verdict is an answer rather than an admission.
    pub fn conclusive(self) -> bool {
        !matches!(self, Self::Unknown)
    }

    /// Whether the budget is known to be exceeded. Only `Over` says so;
    /// `Unknown` must never read as "fine".
    pub fn exceeded(self) -> bool {
        matches!(self, Self::Over)
    }
}

/// Judge a total against the budget, refusing to answer when it cannot.
///
/// The asymmetry is the point. Over-budget survives an incomplete total
/// because unmeasured bytes are still bytes; under-budget does not, because
/// the one thing that could overturn it is the thing that was skipped.
pub fn budget_verdict(total: &MeasuredTotal, budget: u64) -> BudgetVerdict {
    if budget == 0 {
        return BudgetVerdict::Unset;
    }
    // One definition of the boundary, shared with the ordering rule.
    if crate::reclaim_order::over_budget(total.bytes, budget) {
        return BudgetVerdict::Over;
    }
    if total.complete() {
        BudgetVerdict::Within
    } else {
        BudgetVerdict::Unknown
    }
}

pub(crate) fn size_records_path(main_root: &Path) -> PathBuf {
    main_root.join(SIZE_RECORDS_RELPATH)
}

/// Read what has been measured so far.
///
/// Records are a cache, not a ledger. A missing, unreadable, or unparseable
/// file means the same thing an empty set does -- nothing has been measured --
/// and treating it as an error would let a corrupt cache break every
/// `broker status` on the machine.
pub(crate) fn load_size_records(main_root: &Path) -> SizeRecords {
    std::fs::read(size_records_path(main_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SizeRecords>(&bytes).ok())
        .filter(|records| records.schema_version == SIZE_RECORD_SCHEMA_VERSION)
        .unwrap_or_default()
}

/// Write measurements back, replacing the file in one rename.
///
/// A half-written cache would be discarded on the next read anyway, but a
/// torn file also loses every record that *was* good, and each of those cost a
/// full walk to obtain.
pub(crate) fn save_size_records(
    main_root: &Path,
    records: &SizeRecords,
) -> Result<(), BrokerOpError> {
    let path = size_records_path(main_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| crate::BrokerError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(records)?;
    std::fs::write(&temporary, &bytes).map_err(|source| crate::BrokerError::Io {
        path: temporary.clone(),
        source,
    })?;
    std::fs::rename(&temporary, &path).map_err(|source| crate::BrokerError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    fn owned(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_string()).collect()
    }

    #[test]
    fn a_directory_nobody_measured_is_unmeasured_rather_than_empty() {
        let mut total = MeasuredTotal::default();
        total.add_unmeasured();
        assert_eq!(total.bytes, 0);
        assert!(!total.complete());
        assert_eq!(total.unmeasured, 1);
    }

    #[test]
    fn a_floor_over_the_budget_is_still_over_it() {
        let total = MeasuredTotal {
            bytes: 2_048,
            measured: 1,
            unmeasured: 3,
            oldest_measured_at_ms: Some(0),
        };
        assert_eq!(budget_verdict(&total, 1_024), BudgetVerdict::Over);
        assert!(budget_verdict(&total, 1_024).conclusive());
    }

    #[test]
    fn a_floor_under_the_budget_concludes_nothing() {
        let total = MeasuredTotal {
            bytes: 512,
            measured: 1,
            unmeasured: 1,
            oldest_measured_at_ms: Some(0),
        };
        let verdict = budget_verdict(&total, 1_024);
        assert_eq!(verdict, BudgetVerdict::Unknown);
        assert!(!verdict.conclusive());
        assert!(!verdict.exceeded());
    }

    #[test]
    fn the_same_bytes_read_as_within_budget_once_everything_is_measured() {
        let total = MeasuredTotal {
            bytes: 512,
            measured: 2,
            unmeasured: 0,
            oldest_measured_at_ms: Some(0),
        };
        assert_eq!(budget_verdict(&total, 1_024), BudgetVerdict::Within);
    }

    #[test]
    fn an_unset_budget_is_not_a_verdict_about_bytes() {
        let total = MeasuredTotal {
            bytes: u64::MAX,
            measured: 0,
            unmeasured: 9,
            oldest_measured_at_ms: None,
        };
        assert_eq!(budget_verdict(&total, 0), BudgetVerdict::Unset);
        assert!(!budget_verdict(&total, 0).exceeded());
    }

    #[test]
    fn no_two_verdicts_read_alike() {
        let names = [
            BudgetVerdict::Unset.as_str(),
            BudgetVerdict::Within.as_str(),
            BudgetVerdict::Over.as_str(),
            BudgetVerdict::Unknown.as_str(),
        ];
        let unique = names.iter().collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), names.len());
    }

    #[test]
    fn a_total_reports_the_oldest_measurement_that_went_into_it() {
        let mut total = MeasuredTotal::default();
        total.add_measured(10, 5_000);
        total.add_measured(10, 1_000);
        total.add_measured(10, 9_000);
        assert_eq!(total.oldest_measured_at_ms, Some(1_000));
        assert_eq!(total.bytes, 30);
        assert!(total.complete());
    }

    #[test]
    fn warming_closes_the_unmeasured_gap_before_refreshing_an_old_figure() {
        let mut records = SizeRecords::default();
        records.record("/a", 1, 0);
        let paths = owned(&["/a", "/b"]);
        assert_eq!(
            records.next_to_measure(&paths, 100 * HOUR, 24 * HOUR),
            Some("/b".to_string())
        );
    }

    #[test]
    fn warming_then_takes_the_stalest_record() {
        let mut records = SizeRecords::default();
        records.record("/a", 1, 50 * HOUR);
        records.record("/b", 1, 10 * HOUR);
        let paths = owned(&["/a", "/b"]);
        assert_eq!(
            records.next_to_measure(&paths, 100 * HOUR, 24 * HOUR),
            Some("/b".to_string())
        );
    }

    #[test]
    fn warming_stops_once_every_record_is_inside_the_ttl() {
        let mut records = SizeRecords::default();
        records.record("/a", 1, 90 * HOUR);
        records.record("/b", 1, 95 * HOUR);
        let paths = owned(&["/a", "/b"]);
        assert_eq!(records.next_to_measure(&paths, 100 * HOUR, 24 * HOUR), None);
    }

    #[test]
    fn two_never_measured_paths_are_taken_in_the_order_given() {
        let records = SizeRecords::default();
        let paths = owned(&["/a", "/b"]);
        assert_eq!(
            records.next_to_measure(&paths, HOUR, 24 * HOUR),
            Some("/a".to_string())
        );
    }

    #[test]
    fn forgetting_a_path_the_broker_no_longer_tracks() {
        let mut records = SizeRecords::default();
        records.record("/gone", 1, 0);
        records.record("/here", 2, 0);
        let live = BTreeSet::from(["/here".to_string()]);
        assert_eq!(records.retain_paths(&live), 1);
        assert!(records.get("/gone").is_none());
        assert_eq!(records.get("/here").map(|record| record.bytes), Some(2));
    }

    #[test]
    fn a_cache_from_another_schema_reads_as_nothing_measured() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        std::fs::create_dir_all(root.join(".aethyme")).expect("runtime dir");
        std::fs::write(
            size_records_path(root),
            br#"{"schema_version":99,"records":{"/a":{"bytes":5,"measured_at_ms":1}}}"#,
        )
        .expect("write");
        assert!(load_size_records(root).records.is_empty());
    }

    #[test]
    fn records_survive_a_round_trip_through_the_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        let mut records = SizeRecords::default();
        records.record("/a", 42, 7);
        save_size_records(root, &records).expect("save");
        assert_eq!(load_size_records(root), records);
    }
}
