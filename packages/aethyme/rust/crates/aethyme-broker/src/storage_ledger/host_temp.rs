//! Host temp folders: how big the per-user temp folder and `/tmp` are, and
//! whether the volume holding them is short of space.
//!
//! Aethyme points the temp variables of every command it runs for a session
//! at the session scratch ([`crate::command_temp`]), but agents and scripts
//! also write to `/tmp` by name, which no variable redirects. Nothing records
//! who made those entries, so they are reported, never deleted: these are not
//! Aethyme's roots.
//!
//! Only the storage worker measures them, after its pass and at most every
//! [`REFRESH_EVERY`]: a one-level listing (at most [`ENTRY_LIMIT`] entries)
//! and a walk of each entry with one deadline for the whole reading
//! ([`MEASURE_BUDGET`]), never following a symbolic link nor leaving the
//! root's device. Sizes persist per entry (keyed by a hash of its name, so no
//! name is stored), unmeasured entries first and then the oldest measured, so
//! a large folder converges over a few passes. `broker status` reads only the
//! small reading file.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{Budgets, GIB, HOUR, Usage, check::human, now_ms, walk_until, write_atomically};

/// `<host state>/storage-ledger-host-temp/`: the reading and the size cache.
pub const HOST_TEMP_DIRECTORY: &str = "storage-ledger-host-temp";
/// What `broker status` and `storage status` read.
const READING_FILE: &str = "reading.json";
/// Per-entry sizes the next reading resumes from (worker only).
const SIZES_FILE: &str = "sizes.json";
/// Wall time one reading may spend walking, across all roots.
pub const MEASURE_BUDGET: Duration = Duration::from_secs(10);
/// A reading younger than this is not taken again.
pub const REFRESH_EVERY: Duration = Duration::from_secs(10 * 60);
/// Top-level entries listed per root, at most.
pub const ENTRY_LIMIT: usize = 100_000;
/// A reading older than this advises nothing (the worker has not run).
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// One trend sample per root per hour, three days of them.
const SAMPLE_EVERY: Duration = HOUR;
const SAMPLES_PER_ROOT: usize = 72;
/// A volume short of space is advised only when the temp folder on it holds
/// at least this much: below it, the folder is not where the space went.
const SHORT_VOLUME_MIN_BYTES: u64 = GIB;

/// Which host temp folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TempKind {
    /// The per-user temp folder (`getconf DARWIN_USER_TEMP_DIR` on macOS).
    UserTemp,
    /// `/tmp`.
    SystemTmp,
}

impl TempKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::UserTemp => "the per-user temp folder",
            Self::SystemTmp => "/tmp",
        }
    }
}

/// One folder to measure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempRoot {
    pub kind: TempKind,
    pub path: PathBuf,
}

/// One folder's reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootReading {
    pub kind: TempKind,
    pub path: PathBuf,
    /// Top-level entries (at most [`ENTRY_LIMIT`]; `entries_capped` beyond).
    pub entries: u64,
    pub entries_capped: bool,
    /// Allocated bytes of the measured entries: a lower bound when
    /// `partial`.
    pub bytes: u64,
    /// Entries with no size yet (the deadline came first).
    pub unmeasured_entries: u64,
    /// Some entry is unmeasured or measured only up to the deadline, or the
    /// listing was capped.
    pub partial: bool,
    pub free_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    /// Entries walked by this reading, and the time it took.
    pub walked_entries: u64,
    pub measure_ms: u64,
}

/// One trend point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    pub at_ms: u64,
    pub kind: TempKind,
    pub bytes: u64,
    pub entries: u64,
}

/// The persisted reading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    pub updated_at_ms: u64,
    pub roots: Vec<RootReading>,
    /// Hourly samples per root, oldest first.
    #[serde(default)]
    pub history: Vec<Sample>,
}

impl Reading {
    /// The last reading, or an empty one. Reads one small file.
    pub fn load(host_state: &Path) -> Self {
        fs::read(host_state.join(HOST_TEMP_DIRECTORY).join(READING_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Bytes `kind` grew by since the oldest sample within the last day,
    /// and the hours between: the trend an owner reads.
    pub fn growth(&self, kind: TempKind, now: u64) -> Option<(i64, u64)> {
        let day_ago = now.saturating_sub(24 * duration_ms(HOUR));
        let first = self
            .history
            .iter()
            .find(|sample| sample.kind == kind && sample.at_ms >= day_ago)?;
        let current = self.roots.iter().find(|root| root.kind == kind)?;
        let hours = now.saturating_sub(first.at_ms) / duration_ms(HOUR);
        (hours > 0).then(|| {
            (
                i64::try_from(current.bytes).unwrap_or(i64::MAX)
                    - i64::try_from(first.bytes).unwrap_or(i64::MAX),
                hours,
            )
        })
    }
}

/// One entry's last measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct EntrySize {
    bytes: u64,
    measured_at_ms: u64,
    complete: bool,
}

/// Per root path, per entry-name hash.
type Sizes = BTreeMap<String, BTreeMap<String, EntrySize>>;

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The host temp folders, canonical, each once: the per-user temp folder
/// (macOS) and `/tmp`. Never `$TMPDIR`: a broker-run command's is its own
/// run directory.
pub fn default_roots() -> Vec<TempRoot> {
    #[cfg(target_os = "macos")]
    let user_temp = darwin_user_temp_dir();
    #[cfg(not(target_os = "macos"))]
    let user_temp: Option<PathBuf> = None;
    let candidates = user_temp
        .map(|path| TempRoot {
            kind: TempKind::UserTemp,
            path,
        })
        .into_iter()
        .chain(std::iter::once(TempRoot {
            kind: TempKind::SystemTmp,
            path: PathBuf::from("/tmp"),
        }));
    let mut seen = BTreeSet::new();
    candidates
        .into_iter()
        .filter_map(|root| {
            let path = fs::canonicalize(&root.path).ok()?;
            seen.insert(path.clone()).then_some(TempRoot {
                kind: root.kind,
                path,
            })
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn darwin_user_temp_dir() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut buffer = vec![0u8; 1024];
    // SAFETY: the buffer is writable for its length; confstr writes at most
    // that many bytes, NUL included, and returns the length it needed.
    let needed = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if needed == 0 || needed > buffer.len() {
        return None;
    }
    buffer.truncate(needed - 1);
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(&buffer));
    path.is_absolute().then_some(path)
}

fn name_key(name: &std::ffi::OsStr) -> String {
    use sha2::Digest as _;
    use std::os::unix::ffi::OsStrExt as _;
    let mut key = format!("{:x}", sha2::Sha256::digest(name.as_bytes()));
    key.truncate(16);
    key
}

/// Measure one root until `deadline`, resuming from and updating `sizes`.
/// `None` when the root does not exist.
fn measure_root(
    root: &TempRoot,
    deadline: Instant,
    sizes: &mut BTreeMap<String, EntrySize>,
    beat: &mut dyn FnMut(),
) -> Option<RootReading> {
    use std::os::unix::fs::MetadataExt as _;
    let started = Instant::now();
    let metadata = fs::symlink_metadata(&root.path).ok()?;
    if !metadata.is_dir() {
        return None;
    }
    let device = metadata.dev();
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    let mut capped = false;
    if let Ok(listing) = fs::read_dir(&root.path) {
        for entry in listing.flatten() {
            if entries.len() >= ENTRY_LIMIT {
                capped = true;
                break;
            }
            entries.push((name_key(&entry.file_name()), entry.path()));
        }
    }
    let present: BTreeSet<&str> = entries.iter().map(|(key, _)| key.as_str()).collect();
    sizes.retain(|key, _| present.contains(key.as_str()));
    // Unmeasured first, then the oldest complete size, and entries the
    // deadline cut short last, so one huge entry cannot starve the rest.
    entries.sort_by_key(|(key, _)| match sizes.get(key) {
        None => (0, 0),
        Some(size) if size.complete => (1, size.measured_at_ms),
        Some(size) => (2, size.measured_at_ms),
    });
    let now = now_ms();
    let mut walked = 0u64;
    for (key, path) in &entries {
        if Instant::now() >= deadline {
            break;
        }
        let mut pending = vec![path.clone()];
        let mut usage = Usage::default();
        let complete = walk_until(
            &mut pending,
            &mut usage,
            device,
            deadline,
            super::BEAT_EVERY,
            beat,
        );
        walked += 1;
        let previous = sizes.get(key).copied();
        let keep_previous = !complete && previous.is_some_and(|size| size.bytes >= usage.bytes);
        if !keep_previous {
            sizes.insert(
                key.clone(),
                EntrySize {
                    bytes: usage.bytes,
                    measured_at_ms: now,
                    complete,
                },
            );
        } else if let Some(size) = sizes.get_mut(key) {
            // Tried now: it goes to the back of its group.
            size.measured_at_ms = now;
        }
    }
    let unmeasured = entries
        .iter()
        .filter(|(key, _)| !sizes.contains_key(key))
        .count() as u64;
    let incomplete = sizes.values().any(|size| !size.complete);
    let volume = super::check::statvfs(&root.path);
    crate::phase_telemetry::add("storage_host_temp_root_measured", started.elapsed());
    if unmeasured > 0 || incomplete {
        crate::phase_telemetry::add("storage_host_temp_partial", Duration::ZERO);
    }
    Some(RootReading {
        kind: root.kind,
        path: root.path.clone(),
        entries: entries.len() as u64,
        entries_capped: capped,
        bytes: sizes.values().map(|size| size.bytes).sum(),
        unmeasured_entries: unmeasured,
        partial: capped || unmeasured > 0 || incomplete,
        free_bytes: volume.map(|(free, _)| free),
        total_bytes: volume.map(|(_, total)| total).filter(|total| *total > 0),
        walked_entries: walked,
        measure_ms: duration_ms(started.elapsed()),
    })
}

/// Take a reading of `roots` within `budget` and persist it, unless the last
/// one is younger than [`REFRESH_EVERY`] and `force` is off. Worker only.
pub fn refresh(
    host_state: &Path,
    roots: &[TempRoot],
    budget: Duration,
    force: bool,
    beat: &mut dyn FnMut(),
) -> io::Result<Reading> {
    let _span = crate::phase_telemetry::span("storage_host_temp_measure");
    let previous = Reading::load(host_state);
    let now = now_ms();
    if !force
        && previous.updated_at_ms > 0
        && now.saturating_sub(previous.updated_at_ms) < duration_ms(REFRESH_EVERY)
    {
        return Ok(previous);
    }
    let directory = host_state.join(HOST_TEMP_DIRECTORY);
    fs::create_dir_all(&directory)?;
    let mut sizes: Sizes = fs::read(directory.join(SIZES_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let started = Instant::now();
    let mut readings = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        // Each remaining root gets an equal share of what is left.
        let left = budget.saturating_sub(started.elapsed());
        let share = left / u32::try_from(roots.len() - index).unwrap_or(1);
        let key = root.path.to_string_lossy().into_owned();
        let entry_sizes = sizes.entry(key).or_default();
        if let Some(reading) = measure_root(root, Instant::now() + share, entry_sizes, beat) {
            readings.push(reading);
        }
    }
    let measured: BTreeSet<String> = readings
        .iter()
        .map(|reading| reading.path.to_string_lossy().into_owned())
        .collect();
    sizes.retain(|path, _| measured.contains(path));
    let mut history = previous.history;
    for reading in &readings {
        let last = history
            .iter()
            .rev()
            .find(|sample| sample.kind == reading.kind)
            .map_or(0, |sample| sample.at_ms);
        if now.saturating_sub(last) >= duration_ms(SAMPLE_EVERY) {
            history.push(Sample {
                at_ms: now,
                kind: reading.kind,
                bytes: reading.bytes,
                entries: reading.entries,
            });
        }
    }
    for kind in [TempKind::UserTemp, TempKind::SystemTmp] {
        let count = history.iter().filter(|sample| sample.kind == kind).count();
        let mut excess = count.saturating_sub(SAMPLES_PER_ROOT);
        history.retain(|sample| {
            if sample.kind == kind && excess > 0 {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }
    let reading = Reading {
        updated_at_ms: now,
        roots: readings,
        history,
    };
    write_atomically(
        &directory.join(SIZES_FILE),
        &serde_json::to_vec(&sizes).map_err(io::Error::other)?,
    )?;
    write_atomically(
        &directory.join(READING_FILE),
        &serde_json::to_vec(&reading).map_err(io::Error::other)?,
    )?;
    Ok(reading)
}

/// `storage.host-temp-large`: a host temp folder holds more than its
/// threshold (bytes or entries), or the volume holding a sizeable one is
/// below its free-space floor. Reads only; nothing here deletes.
pub fn advice(reading: &Reading, budgets: &Budgets, now: u64) -> Option<crate::StatusAdvice> {
    if reading.updated_at_ms == 0
        || now.saturating_sub(reading.updated_at_ms) > duration_ms(STALE_AFTER)
    {
        return None;
    }
    let mut short_volume = false;
    let mut evidence = Vec::new();
    for root in &reading.roots {
        let large = root.bytes >= budgets.host_temp_advice_bytes
            || root.entries >= budgets.host_temp_advice_entries;
        let short = root.bytes >= SHORT_VOLUME_MIN_BYTES
            && root
                .free_bytes
                .is_some_and(|free| free < budgets.host_temp_free_floor_bytes);
        if !large && !short {
            continue;
        }
        short_volume |= short;
        let at_least = if root.partial { "at least " } else { "" };
        let growth = reading
            .growth(root.kind, now)
            .filter(|(bytes, _)| *bytes > 0)
            .map(|(bytes, hours)| {
                format!(
                    ", up {} in {hours} h",
                    human(u64::try_from(bytes).unwrap_or(0))
                )
            })
            .unwrap_or_default();
        evidence.push(format!(
            "{} holds {at_least}{} in {}{} entries{growth}; its volume has {} free",
            root.kind.label(),
            human(root.bytes),
            root.entries,
            if root.entries_capped { "+" } else { "" },
            root.free_bytes.map_or_else(|| "unknown".to_string(), human),
        ));
    }
    if evidence.is_empty() {
        return None;
    }
    Some(crate::StatusAdvice {
        id: "storage.host-temp-large",
        severity: if short_volume {
            crate::StatusAdviceSeverity::Warning
        } else {
            crate::StatusAdviceSeverity::Notice
        },
        reason: "a host temp folder is large or its volume is short of space; Aethyme did not create these entries and never deletes them. Session work belongs in the session scratch ($AETHYME_SESSION_SCRATCH_DIR, or $TMPDIR inside a broker-run command), which goes with the session; nothing removes what is written to /tmp by name",
        summary: format!(
            "{} host temp folder(s) large or on a volume short of space",
            evidence.len()
        ),
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands: vec!["aethyme storage status".into()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(path: &Path) -> TempRoot {
        TempRoot {
            kind: TempKind::SystemTmp,
            path: path.to_path_buf(),
        }
    }

    fn reading(bytes: u64, entries: u64, free: u64) -> Reading {
        Reading {
            updated_at_ms: now_ms(),
            roots: vec![RootReading {
                kind: TempKind::SystemTmp,
                path: PathBuf::from("/tmp"),
                entries,
                entries_capped: false,
                bytes,
                unmeasured_entries: 0,
                partial: false,
                free_bytes: Some(free),
                total_bytes: None,
                walked_entries: entries,
                measure_ms: 1,
            }],
            history: Vec::new(),
        }
    }

    /// The advice appears above each threshold and not below, says the
    /// entries are not Aethyme's to delete, and names the session scratch.
    #[test]
    fn advice_appears_above_the_thresholds_and_not_below() {
        let budgets = Budgets::default();
        let plenty = budgets.host_temp_free_floor_bytes * 4;
        let now = now_ms();
        let below = reading(budgets.host_temp_advice_bytes - 1, 10, plenty);
        assert!(advice(&below, &budgets, now).is_none());
        let large = reading(budgets.host_temp_advice_bytes, 10, plenty);
        let row = advice(&large, &budgets, now).expect("large bytes advise");
        assert_eq!(row.id, "storage.host-temp-large");
        assert!(matches!(row.severity, crate::StatusAdviceSeverity::Notice));
        assert!(row.reason.contains("never deletes"), "{}", row.reason);
        assert!(row.reason.contains("AETHYME_SESSION_SCRATCH_DIR"));
        assert!(
            row.evidence[0].starts_with("/tmp holds"),
            "{:?}",
            row.evidence
        );
        let many = reading(1, budgets.host_temp_advice_entries, plenty);
        assert!(
            advice(&many, &budgets, now).is_some(),
            "many entries advise"
        );
        // Short of space: a warning, but only when the folder is sizeable.
        let short = reading(
            SHORT_VOLUME_MIN_BYTES,
            10,
            budgets.host_temp_free_floor_bytes - 1,
        );
        let row = advice(&short, &budgets, now).expect("short volume advises");
        assert!(matches!(row.severity, crate::StatusAdviceSeverity::Warning));
        let tiny = reading(SHORT_VOLUME_MIN_BYTES - 1, 10, 1);
        assert!(advice(&tiny, &budgets, now).is_none());
        // Configured thresholds win; a stale reading advises nothing.
        let raised = Budgets {
            host_temp_advice_bytes: u64::MAX,
            ..Budgets::default()
        };
        assert!(advice(&large, &raised, now).is_none());
        let mut stale = large.clone();
        stale.updated_at_ms = now - duration_ms(STALE_AFTER) - 1;
        assert!(advice(&stale, &budgets, now).is_none());
    }

    /// A reading counts and sizes the entries, persists them, and resumes:
    /// the second reading walks only what changed order, never deletes.
    #[test]
    fn reading_counts_entries_and_bytes_and_deletes_nothing() {
        let host = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        for index in 0..5 {
            fs::write(temp.path().join(format!("log-{index}")), vec![1u8; 8192]).unwrap();
        }
        fs::create_dir_all(temp.path().join("checkout/src")).unwrap();
        fs::write(temp.path().join("checkout/src/a"), vec![1u8; 65536]).unwrap();
        let roots = [root(temp.path())];
        let reading = refresh(
            host.path(),
            &roots,
            Duration::from_secs(30),
            true,
            &mut || {},
        )
        .unwrap();
        let measured = &reading.roots[0];
        assert_eq!(measured.entries, 6);
        assert!(!measured.partial, "{measured:?}");
        assert!(measured.bytes >= 5 * 8192 + 65536, "{measured:?}");
        assert_eq!(Reading::load(host.path()), reading);
        assert_eq!(
            fs::read_dir(temp.path()).unwrap().count(),
            6,
            "nothing deleted"
        );
        // Within the refresh interval the stored reading is returned as is.
        fs::write(temp.path().join("late"), b"x").unwrap();
        let again = refresh(
            host.path(),
            &roots,
            Duration::from_secs(30),
            false,
            &mut || {},
        )
        .unwrap();
        assert_eq!(again, reading);
        // A missing root is skipped, not an error.
        let gone = refresh(
            host.path(),
            &[root(&temp.path().join("missing"))],
            Duration::from_secs(1),
            true,
            &mut || {},
        )
        .unwrap();
        assert!(gone.roots.is_empty());
    }

    /// The deadline bounds a reading: an exhausted budget walks nothing,
    /// reports the entries unmeasured and the reading partial, and the next
    /// reading with time left resumes and completes it.
    #[test]
    fn reading_is_bounded_by_its_deadline() {
        let host = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        for index in 0..50 {
            let directory = temp.path().join(format!("store-{index}"));
            fs::create_dir_all(&directory).unwrap();
            for file in 0..20 {
                fs::write(directory.join(format!("f{file}")), b"data").unwrap();
            }
        }
        let roots = [root(temp.path())];
        let started = Instant::now();
        let cut = refresh(host.path(), &roots, Duration::ZERO, true, &mut || {}).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        let measured = &cut.roots[0];
        assert_eq!(measured.entries, 50, "listing is counted regardless");
        assert_eq!(measured.walked_entries, 0, "{measured:?}");
        assert_eq!(measured.unmeasured_entries, 50);
        assert!(measured.partial);
        let full = refresh(
            host.path(),
            &roots,
            Duration::from_secs(30),
            true,
            &mut || {},
        )
        .unwrap();
        assert!(!full.roots[0].partial, "{:?}", full.roots[0]);
        assert_eq!(full.roots[0].unmeasured_entries, 0);
    }

    /// Advice reads only: no file under the host state changes.
    #[test]
    fn status_advice_does_not_write() {
        let host = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("big"), vec![1u8; 4096]).unwrap();
        refresh(
            host.path(),
            &[root(temp.path())],
            Duration::from_secs(5),
            true,
            &mut || {},
        )
        .unwrap();
        fs::write(
            host.path().join(super::super::BUDGET_FILE),
            "[budget]\nhost_temp_advice_bytes = 1\n",
        )
        .unwrap();
        let snapshot = |path: &Path| -> Vec<(PathBuf, u64, std::time::SystemTime)> {
            let mut pending = vec![path.to_path_buf()];
            let mut seen = Vec::new();
            while let Some(next) = pending.pop() {
                let metadata = fs::symlink_metadata(&next).unwrap();
                if metadata.is_dir() {
                    pending.extend(fs::read_dir(&next).unwrap().flatten().map(|e| e.path()));
                }
                seen.push((next, metadata.len(), metadata.modified().unwrap()));
            }
            seen.sort();
            seen
        };
        let before = snapshot(host.path());
        let advice = super::super::cli::status_advice(host.path());
        assert!(
            advice.iter().any(|row| row.id == "storage.host-temp-large"),
            "{advice:?}"
        );
        let status = super::super::cli::status_value(host.path());
        assert_eq!(status["host_temp"]["roots"][0]["entries"], 1);
        assert_eq!(
            snapshot(host.path()),
            before,
            "status wrote under host state"
        );
    }
}
