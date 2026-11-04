//! Observational record of prepare-time shared-cache rotation recovery (#1236).
//!
//! `submit prepare` writes one small JSON record per cache root and
//! repository after every recovery slice and when an attempt ends, so that
//! `resources pressure` and `doctor` can show which retired generation is
//! being recovered, how many entries all attempts have unlinked, what an
//! attempt last counted as remaining, and when recovery last made progress.
//!
//! The record lives in the broker's host-state directory, next to the host
//! resource database, never inside the preparation-cache root: that root's
//! layout is validated strictly and must not gain unknown entries. The record
//! is never recovery authority. Readers load it read-only, with bounded
//! sizes and counts, and never walk a cache store; a missing or unreadable
//! record reads as "no recovery recorded".
#![cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Directory, beside the host resource database, holding the records.
pub(crate) const DIRECTORY: &str = "preparation-rotation-recovery";
const SCHEMA: &str = "aethyme.preparation-rotation-recovery/v1";
const MAX_RECORD_BYTES: u64 = 16 * 1024;
/// Directory entries a reader considers, and records it parses.
const MAX_SCANNED_ENTRIES: usize = 512;
const MAX_RECORDS: usize = 64;
/// An in-progress record not updated for this long belongs to an attempt
/// that is no longer running. Slices update it at least every few seconds.
pub(crate) const IN_PROGRESS_STALE_AFTER_MS: i64 = 120_000;
/// Completed recoveries stay visible for a day, then readers omit them.
const COMPLETE_VISIBLE_FOR_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RotationRecoveryState {
    /// An attempt is draining the retired store now.
    InProgress,
    /// The last attempt ended, or stopped updating, before recovery finished.
    /// The next `submit prepare` resumes where it stopped.
    Stalled,
    /// The retired store is gone and the rotation debt is cleared.
    Complete,
}

/// One cache root and repository's recovery, as stored and as reported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RotationRecoveryRecord {
    pub schema: String,
    /// The preparation-cache root the retired store lives under.
    pub cache_root: PathBuf,
    /// Repository identity: the store's directory name under the cache root.
    pub repository: String,
    /// Retired store name for the generation being recovered; null when the
    /// attempt could not read it from the in-cache debt record.
    pub retired_generation: Option<String>,
    pub state: RotationRecoveryState,
    /// Entries unlinked for this generation, summed across attempts.
    pub entries_unlinked_total: u64,
    /// `submit prepare` attempts that recovered this generation.
    pub attempts: u64,
    /// Entries the last incomplete attempt counted in the retired store;
    /// null when uncounted, and 0 once recovery is complete.
    pub remaining_entries: Option<u64>,
    /// True when `remaining_entries` is a lower bound: the count stopped at
    /// its entry or time limit.
    pub remaining_entries_is_lower_bound: bool,
    /// When a slice last unlinked anything (milliseconds since the epoch).
    pub last_progress_at_ms: Option<i64>,
    pub updated_at_ms: i64,
    /// Why the last incomplete attempt stopped.
    pub last_stop_reason: Option<String>,
}

fn record_name(cache_root: &Path, repository: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cache_root.as_os_str().as_encoded_bytes());
    hasher.update([0]);
    hasher.update(repository.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{hex}.json")
}

fn state_directory(resource_db_path: &Path) -> Option<PathBuf> {
    resource_db_path
        .parent()
        .map(|parent| parent.join(DIRECTORY))
}

/// Writer side, owned by one recovery call while it holds the repository's
/// preparation-cache lease.
pub(crate) struct RecoveryRecorder {
    path: Option<PathBuf>,
    record: RotationRecoveryRecord,
    /// A record for this cache root and repository existed before this
    /// attempt, so completion must be written even with nothing unlinked.
    had_record: bool,
}

impl RecoveryRecorder {
    pub(crate) fn begin(
        resource_db_path: &Path,
        cache_root: &Path,
        repository: &str,
        retired_generation: Option<String>,
        now_ms: i64,
    ) -> Self {
        let path = state_directory(resource_db_path)
            .map(|directory| directory.join(record_name(cache_root, repository)));
        let previous = path.as_deref().and_then(|path| read_record(path).ok());
        let had_record = previous.is_some();
        let continued = previous.filter(|previous| {
            previous.state != RotationRecoveryState::Complete
                && previous.cache_root == cache_root
                && previous.repository == repository
                && (retired_generation.is_none()
                    || previous.retired_generation == retired_generation)
        });
        let record = match continued {
            Some(mut previous) => {
                previous.attempts = previous.attempts.saturating_add(1);
                if previous.retired_generation.is_none() {
                    previous.retired_generation = retired_generation;
                }
                previous.updated_at_ms = now_ms;
                previous
            }
            None => RotationRecoveryRecord {
                schema: SCHEMA.into(),
                cache_root: cache_root.to_path_buf(),
                repository: repository.into(),
                retired_generation,
                state: RotationRecoveryState::InProgress,
                entries_unlinked_total: 0,
                attempts: 1,
                remaining_entries: None,
                remaining_entries_is_lower_bound: false,
                last_progress_at_ms: None,
                updated_at_ms: now_ms,
                last_stop_reason: None,
            },
        };
        Self {
            path,
            record,
            had_record,
        }
    }

    fn worth_recording(&self) -> bool {
        self.had_record
            || self.record.retired_generation.is_some()
            || self.record.entries_unlinked_total > 0
    }

    /// Record one finished slice that will be followed by another.
    pub(crate) fn slice(&mut self, entries_unlinked: usize, now_ms: i64) {
        self.add(entries_unlinked, now_ms);
        self.record.state = RotationRecoveryState::InProgress;
        self.persist();
    }

    pub(crate) fn complete(&mut self, entries_unlinked: usize, now_ms: i64) {
        self.add(entries_unlinked, now_ms);
        self.record.state = RotationRecoveryState::Complete;
        self.record.remaining_entries = Some(0);
        self.record.remaining_entries_is_lower_bound = false;
        self.record.last_stop_reason = None;
        self.persist();
    }

    pub(crate) fn incomplete(
        &mut self,
        entries_unlinked: usize,
        remaining: Option<(u64, bool)>,
        reason: String,
        now_ms: i64,
    ) {
        self.add(entries_unlinked, now_ms);
        self.record.state = RotationRecoveryState::Stalled;
        self.record.remaining_entries = remaining.map(|(entries, _)| entries);
        self.record.remaining_entries_is_lower_bound =
            remaining.is_some_and(|(_, lower_bound)| lower_bound);
        self.record.last_stop_reason = Some(reason);
        self.persist();
    }

    fn add(&mut self, entries_unlinked: usize, now_ms: i64) {
        self.record.entries_unlinked_total = self
            .record
            .entries_unlinked_total
            .saturating_add(entries_unlinked as u64);
        if entries_unlinked > 0 {
            self.record.last_progress_at_ms = Some(now_ms);
        }
        self.record.updated_at_ms = now_ms;
    }

    fn persist(&mut self) {
        if !self.worth_recording() {
            return;
        }
        let Some(path) = self.path.as_deref() else {
            return;
        };
        // Diagnostics must never fail recovery or prepare.
        crate::warn_unrecorded(
            "record preparation-cache rotation recovery progress",
            write_record(path, &self.record),
        );
        self.had_record = true;
    }

    #[cfg(test)]
    pub(crate) fn record(&self) -> &RotationRecoveryRecord {
        &self.record
    }
}

fn write_record(path: &Path, record: &RotationRecoveryRecord) -> std::io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| std::io::Error::other("recovery record has no directory"))?;
    std::fs::create_dir_all(directory)?;
    let bytes = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("record"),
        std::process::id()
    ));
    let written = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if written.is_err() {
        crate::warn_unrecorded(
            "remove a partial preparation-cache recovery record",
            std::fs::remove_file(&temporary),
        );
    }
    written
}

fn read_record(path: &Path) -> std::io::Result<RotationRecoveryRecord> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "recovery record is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let record: RotationRecoveryRecord = serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if record.schema != SCHEMA {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "recovery record has an unknown schema",
        ));
    }
    Ok(record)
}

/// Read every recorded recovery for this host, read-only and bounded.
///
/// Opens only the record directory and its small files: it never creates
/// state and never walks a cache store. Records that are missing, oversized
/// or malformed are skipped. An in-progress record that stopped updating is
/// reported as stalled, and completed recoveries older than a day are
/// omitted.
pub(crate) fn read_records(resource_db_path: &Path, now_ms: i64) -> Vec<RotationRecoveryRecord> {
    let Some(directory) = state_directory(resource_db_path) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut names: Vec<_> = entries
        .take(MAX_SCANNED_ENTRIES)
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| {
            name.to_str()
                .is_some_and(|name| !name.starts_with('.') && name.ends_with(".json"))
        })
        .collect();
    names.sort();
    let mut records: Vec<_> = names
        .into_iter()
        .take(MAX_RECORDS)
        .filter_map(|name| read_record(&directory.join(name)).ok())
        .filter_map(|mut record| {
            let age = now_ms.saturating_sub(record.updated_at_ms);
            match record.state {
                RotationRecoveryState::Complete if age > COMPLETE_VISIBLE_FOR_MS => None,
                RotationRecoveryState::InProgress if age > IN_PROGRESS_STALE_AFTER_MS => {
                    record.state = RotationRecoveryState::Stalled;
                    Some(record)
                }
                _ => Some(record),
            }
        })
        .collect();
    records.sort_by(|left, right| {
        left.cache_root
            .cmp(&right.cache_root)
            .then_with(|| left.repository.cmp(&right.repository))
    });
    records
}

/// One human line per record, for `doctor`.
pub fn describe(record: &RotationRecoveryRecord) -> String {
    let state = match record.state {
        RotationRecoveryState::InProgress => "in progress",
        RotationRecoveryState::Stalled => "stalled",
        RotationRecoveryState::Complete => "complete",
    };
    let remaining = match (record.state, record.remaining_entries) {
        (RotationRecoveryState::Complete, _) => String::new(),
        (_, Some(entries)) if record.remaining_entries_is_lower_bound => {
            format!(", at least {entries} remaining")
        }
        (_, Some(entries)) => format!(", {entries} remaining"),
        (_, None) => ", remaining not counted".into(),
    };
    format!(
        "{state}: repository {} generation {}: {} entries unlinked over {} attempt(s){remaining}",
        &record.repository[..record.repository.len().min(12)],
        record.retired_generation.as_deref().unwrap_or("unknown"),
        record.entries_unlinked_total,
        record.attempts,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPOSITORY: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let resource_db = tmp.path().join("host-state/host-resources.db");
        // The cache root deliberately does not exist: a reader that touched
        // the store would fail or change what it reports.
        let cache_root = tmp.path().join("absent-cache-root");
        (tmp, resource_db, cache_root)
    }

    #[test]
    fn readers_report_nothing_and_create_nothing_without_records() {
        let (tmp, resource_db, _cache_root) = fixture();
        assert!(read_records(&resource_db, 1_000).is_empty());
        assert!(!tmp.path().join("host-state").exists());
    }

    #[test]
    fn a_recovery_without_debt_writes_no_record() {
        let (tmp, resource_db, cache_root) = fixture();
        let mut recorder = RecoveryRecorder::begin(&resource_db, &cache_root, REPOSITORY, None, 1);
        recorder.slice(0, 2);
        recorder.complete(0, 3);
        assert!(!tmp.path().join("host-state").exists());
        assert!(read_records(&resource_db, 4).is_empty());
    }

    #[test]
    fn attempts_of_one_generation_accumulate_and_a_new_generation_restarts() {
        let (_tmp, resource_db, cache_root) = fixture();
        let generation = Some("retired-a".to_string());
        let mut first = RecoveryRecorder::begin(
            &resource_db,
            &cache_root,
            REPOSITORY,
            generation.clone(),
            10,
        );
        first.slice(40, 11);
        first.incomplete(0, Some((900, true)), "deadline".into(), 12);
        let records = read_records(&resource_db, 13);
        assert_eq!(records.len(), 1);
        let stalled = &records[0];
        assert_eq!(stalled.state, RotationRecoveryState::Stalled);
        assert_eq!(stalled.entries_unlinked_total, 40);
        assert_eq!(stalled.remaining_entries, Some(900));
        assert!(stalled.remaining_entries_is_lower_bound);
        assert_eq!(stalled.last_progress_at_ms, Some(11));
        assert_eq!(stalled.last_stop_reason.as_deref(), Some("deadline"));

        let mut second =
            RecoveryRecorder::begin(&resource_db, &cache_root, REPOSITORY, generation, 20);
        second.slice(60, 21);
        second.complete(0, 22);
        let complete = &read_records(&resource_db, 23)[0];
        assert_eq!(complete.state, RotationRecoveryState::Complete);
        assert_eq!(complete.entries_unlinked_total, 100);
        assert_eq!(complete.attempts, 2);
        assert_eq!(complete.remaining_entries, Some(0));

        let mut stalled_again = RecoveryRecorder::begin(
            &resource_db,
            &cache_root,
            REPOSITORY,
            Some("retired-a".into()),
            30,
        );
        stalled_again.slice(5, 31);
        stalled_again.incomplete(0, None, "deadline".into(), 32);
        let mut next_generation = RecoveryRecorder::begin(
            &resource_db,
            &cache_root,
            REPOSITORY,
            Some("retired-b".into()),
            40,
        );
        assert_eq!(next_generation.record().entries_unlinked_total, 0);
        assert_eq!(next_generation.record().attempts, 1);
        next_generation.slice(1, 41);
        assert_eq!(
            read_records(&resource_db, 42)[0]
                .retired_generation
                .as_deref(),
            Some("retired-b")
        );
    }

    #[test]
    fn readers_age_records_and_skip_malformed_ones_without_touching_the_store() {
        let (_tmp, resource_db, cache_root) = fixture();
        let mut recorder = RecoveryRecorder::begin(
            &resource_db,
            &cache_root,
            REPOSITORY,
            Some("retired-a".into()),
            0,
        );
        recorder.slice(3, 0);
        let fresh = read_records(&resource_db, IN_PROGRESS_STALE_AFTER_MS);
        assert_eq!(fresh[0].state, RotationRecoveryState::InProgress);
        assert_eq!(fresh[0].cache_root, cache_root);
        let abandoned = read_records(&resource_db, IN_PROGRESS_STALE_AFTER_MS + 1);
        assert_eq!(abandoned[0].state, RotationRecoveryState::Stalled);
        recorder.complete(0, 0);
        assert_eq!(read_records(&resource_db, COMPLETE_VISIBLE_FOR_MS).len(), 1);
        assert!(read_records(&resource_db, COMPLETE_VISIBLE_FOR_MS + 1).is_empty());

        let directory = resource_db.parent().unwrap().join(DIRECTORY);
        std::fs::write(directory.join("malformed.json"), b"{not json").unwrap();
        std::fs::write(
            directory.join("oversized.json"),
            vec![b' '; MAX_RECORD_BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(read_records(&resource_db, 0).len(), 1);
        assert!(!cache_root.exists(), "readers never create or walk a store");
    }

    #[test]
    fn describe_names_state_progress_and_remaining() {
        let (_tmp, resource_db, cache_root) = fixture();
        let mut recorder = RecoveryRecorder::begin(
            &resource_db,
            &cache_root,
            REPOSITORY,
            Some("retired-a".into()),
            0,
        );
        recorder.slice(7, 1);
        recorder.incomplete(0, Some((12, false)), "deadline".into(), 2);
        assert_eq!(
            describe(recorder.record()),
            "stalled: repository abcdef012345 generation retired-a: 7 entries unlinked over 1 attempt(s), 12 remaining"
        );
    }
}
