//! Which pool entries were already hashed, so a later preparation need not
//! read them again.
//!
//! An entry is hashed against its name before its first use. The result is
//! kept in one small record inside the pool, bound to the entry's device,
//! inode, size, modification and change time (nanoseconds). An entry is
//! trusted only while every one of those still matches: replacing,
//! truncating or overwriting it changes the change time, and a renumbered
//! device merely causes another hash. A result is kept only when the entry
//! had been unchanged for [`SETTLE`] when it was hashed, so a write in the
//! same timestamp tick, or data the volume had not flushed, is never
//! remembered, and for [`MAX_AGE`], so bytes damaged on disk without a
//! metadata change are still found.
//!
//! The record only ever saves reads. One that is missing, unreadable, too
//! large, of another format, a link, or writable by others counts as empty.
//! Concurrent preparations replace it atomically; the loser's results are
//! hashed again. It is trusted as far as the pool's owner: anyone able to
//! rewrite it as that user can rewrite the checkouts too.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The record's name in the pool. Every entry lives in a two-digit bucket
/// directory, so a file here is never taken for an entry.
pub(crate) const RECORD_FILE: &str = ".aethyme-verified-entries.json";

const RECORD_VERSION: u32 = 1;
#[cfg(unix)]
const MAX_RECORD_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RECORDED_ENTRIES: usize = 4096;

/// How long an entry must have been unchanged when hashed to be recorded.
pub(crate) const SETTLE: Duration = Duration::from_secs(60);

/// How long a recorded result stands before the entry is hashed again.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// What must be unchanged for an earlier hash of a file to still hold.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntryIdentity {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}

impl EntryIdentity {
    #[cfg(unix)]
    pub(crate) fn of(metadata: &fs::Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            len: metadata.len(),
            mtime: metadata.mtime(),
            mtime_ns: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_ns: metadata.ctime_nsec(),
        })
    }

    /// No strict identity here, so nothing is ever trusted unread.
    #[cfg(not(unix))]
    pub(crate) fn of(_metadata: &fs::Metadata) -> Option<Self> {
        None
    }

    fn unchanged_for(&self, age: Duration, now_ms: i64) -> bool {
        let changed_ms = self
            .ctime
            .saturating_mul(1000)
            .saturating_add(self.ctime_ns / 1_000_000);
        now_ms.saturating_sub(changed_ms) >= millis(age)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Verified {
    identity: EntryIdentity,
    verified_at_ms: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    entries: BTreeMap<String, Verified>,
}

/// Where an entry's verification was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KnownFrom {
    ThisPass,
    Record,
}

/// One pass's view of the record and what it adds to it.
pub(crate) struct VerifiedEntries {
    recorded: BTreeMap<String, Verified>,
    this_pass: HashMap<String, EntryIdentity>,
    learned: BTreeMap<String, Verified>,
    rejected: BTreeSet<String>,
    settle: Duration,
    /// A record was present but not usable.
    pub(crate) record_ignored: bool,
}

impl VerifiedEntries {
    pub(crate) fn load(pool: &Path, settle: Duration, now_ms: i64) -> Self {
        let (recorded, record_ignored) = match read_record(&pool.join(RECORD_FILE), now_ms) {
            Ok(entries) => (entries, false),
            Err(error) => (BTreeMap::new(), error.kind() != io::ErrorKind::NotFound),
        };
        Self {
            recorded,
            this_pass: HashMap::new(),
            learned: BTreeMap::new(),
            rejected: BTreeSet::new(),
            settle,
            record_ignored,
        }
    }

    /// Whether the entry named `digest`, now `identity`, was hashed before.
    pub(crate) fn known(&self, digest: &str, identity: &EntryIdentity) -> Option<KnownFrom> {
        if self.this_pass.get(digest) == Some(identity) {
            return Some(KnownFrom::ThisPass);
        }
        self.recorded
            .get(digest)
            .is_some_and(|verified| verified.identity == *identity)
            .then_some(KnownFrom::Record)
    }

    /// This pass published the entry after hashing its bytes. Too recent to
    /// record; the next pass hashes the entry itself.
    pub(crate) fn published(&mut self, digest: &str, identity: EntryIdentity) {
        self.this_pass.insert(digest.to_owned(), identity);
    }

    /// The entry hashed to its name in a read begun at `now_ms`, with
    /// `identity` both before and after it.
    pub(crate) fn verified(&mut self, digest: &str, identity: EntryIdentity, now_ms: i64) {
        self.this_pass.insert(digest.to_owned(), identity);
        if identity.unchanged_for(self.settle, now_ms) {
            self.rejected.remove(digest);
            self.learned.insert(
                digest.to_owned(),
                Verified {
                    identity,
                    verified_at_ms: now_ms,
                },
            );
        }
    }

    pub(crate) fn rejected(&mut self, digest: &str) {
        self.this_pass.remove(digest);
        self.learned.remove(digest);
        self.recorded.remove(digest);
        self.rejected.insert(digest.to_owned());
    }

    /// Merge what this pass learned into the record on disk, through
    /// `temporary` beside it. Nothing is written when nothing was learned.
    pub(crate) fn save(&self, pool: &Path, temporary: &Path, now_ms: i64) -> io::Result<()> {
        if self.learned.is_empty() {
            return Ok(());
        }
        let path = pool.join(RECORD_FILE);
        let mut entries = read_record(&path, now_ms).unwrap_or_default();
        entries.extend(
            self.learned
                .iter()
                .map(|(digest, verified)| (digest.clone(), *verified)),
        );
        entries.retain(|digest, _| !self.rejected.contains(digest));
        if entries.len() > MAX_RECORDED_ENTRIES {
            let mut newest: Vec<_> = entries.into_iter().collect();
            newest.sort_by_key(|(_, verified)| std::cmp::Reverse(verified.verified_at_ms));
            newest.truncate(MAX_RECORDED_ENTRIES);
            entries = newest.into_iter().collect();
        }
        let bytes = serde_json::to_vec(&Record {
            version: RECORD_VERSION,
            entries,
        })
        .map_err(io::Error::other)?;
        write_new(temporary, &bytes)?;
        fs::rename(temporary, &path)
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// The record's current entries. An error means it is absent or unusable.
fn read_record(path: &Path, now_ms: i64) -> io::Result<BTreeMap<String, Verified>> {
    let bytes = read_bounded(path)?;
    let record: Record = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if record.version != RECORD_VERSION {
        return Err(io::Error::other("another record format"));
    }
    let mut entries = record.entries;
    entries.retain(|_, verified| {
        let age = now_ms.saturating_sub(verified.verified_at_ms);
        (0..=millis(MAX_AGE)).contains(&age)
    });
    Ok(entries)
}

#[cfg(unix)]
fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let owner = unsafe { libc::geteuid() };
    if !metadata.is_file()
        // Zero links: another pass replaced the record since the open.
        || metadata.nlink() > 1
        || metadata.uid() != owner
        || metadata.mode() & 0o022 != 0
        || metadata.len() > MAX_RECORD_BYTES
    {
        return Err(io::Error::other("not a private regular record"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(io::Error::other("the record is too large"));
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_bounded(_path: &Path) -> io::Result<Vec<u8>> {
    Err(io::ErrorKind::NotFound.into())
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    io::Write::write_all(&mut options.open(path)?, bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const NOW: i64 = 4_000_000_000_000;

    fn digest(index: usize) -> String {
        format!("{index:064x}")
    }

    fn identity(inode: u64) -> EntryIdentity {
        EntryIdentity {
            dev: 7,
            ino: inode,
            len: 100_000,
            mtime: 1,
            mtime_ns: 2,
            ctime: 3,
            ctime_ns: 4,
        }
    }

    fn save(pool: &Path, indices: std::ops::Range<usize>, now_ms: i64) {
        let mut entries = VerifiedEntries::load(pool, SETTLE, now_ms);
        for index in indices {
            entries.verified(&digest(index), identity(index as u64), now_ms);
        }
        entries.save(pool, &pool.join("temporary"), now_ms).unwrap();
    }

    fn recorded(pool: &Path, index: usize, identity: &EntryIdentity, now_ms: i64) -> bool {
        VerifiedEntries::load(pool, SETTLE, now_ms).known(&digest(index), identity)
            == Some(KnownFrom::Record)
    }

    #[test]
    fn every_identity_field_must_match() {
        let pool = tempfile::tempdir().unwrap();
        save(pool.path(), 1..2, NOW);
        let good = identity(1);
        assert!(recorded(pool.path(), 1, &good, NOW));
        assert!(!recorded(pool.path(), 2, &good, NOW), "another name");
        for changed in [
            EntryIdentity { dev: 8, ..good },
            EntryIdentity { ino: 9, ..good },
            EntryIdentity { len: 9, ..good },
            EntryIdentity { mtime: 9, ..good },
            EntryIdentity {
                mtime_ns: 9,
                ..good
            },
            EntryIdentity { ctime: 9, ..good },
            EntryIdentity {
                ctime_ns: 9,
                ..good
            },
        ] {
            assert!(!recorded(pool.path(), 1, &changed, NOW), "{changed:?}");
        }
    }

    #[test]
    fn a_result_expires_and_one_from_the_future_is_ignored() {
        let pool = tempfile::tempdir().unwrap();
        save(pool.path(), 1..2, NOW);
        assert!(recorded(
            pool.path(),
            1,
            &identity(1),
            NOW + millis(MAX_AGE)
        ));
        assert!(!recorded(
            pool.path(),
            1,
            &identity(1),
            NOW + millis(MAX_AGE) + 1
        ));
        assert!(!recorded(pool.path(), 1, &identity(1), NOW - 1));
    }

    #[test]
    fn a_recently_changed_entry_is_known_this_pass_only() {
        let pool = tempfile::tempdir().unwrap();
        let fresh = EntryIdentity {
            ctime: NOW / 1000 - 59,
            ctime_ns: 0,
            ..identity(1)
        };
        let mut entries = VerifiedEntries::load(pool.path(), SETTLE, NOW);
        entries.verified(&digest(1), fresh, NOW);
        assert_eq!(entries.known(&digest(1), &fresh), Some(KnownFrom::ThisPass));
        entries
            .save(pool.path(), &pool.path().join("temporary"), NOW)
            .unwrap();
        assert!(!pool.path().join(RECORD_FILE).exists());
        let settled = EntryIdentity {
            ctime: NOW / 1000 - 60,
            ..fresh
        };
        entries.verified(&digest(1), settled, NOW);
        entries
            .save(pool.path(), &pool.path().join("temporary"), NOW)
            .unwrap();
        assert!(recorded(pool.path(), 1, &settled, NOW));
    }

    #[test]
    fn an_unusable_record_is_ignored_and_replaced() {
        let pool = tempfile::tempdir().unwrap();
        let path = pool.path().join(RECORD_FILE);
        save(pool.path(), 1..2, NOW);
        let good = fs::read(&path).unwrap();
        let mut older: serde_json::Value = serde_json::from_slice(&good).unwrap();
        older["version"] = 0.into();
        let mut extra: serde_json::Value = serde_json::from_slice(&good).unwrap();
        extra["entries"][digest(1)]["identity"]
            .as_object_mut()
            .unwrap()
            .remove("ctime_ns");
        // Valid JSON, so only the size refuses it.
        let mut oversized = good.clone();
        oversized.resize(MAX_RECORD_BYTES as usize + 1, b' ');
        for bytes in [
            b"not json".to_vec(),
            Vec::new(),
            good[..good.len() / 2].to_vec(),
            serde_json::to_vec(&older).unwrap(),
            serde_json::to_vec(&extra).unwrap(),
            oversized,
        ] {
            fs::write(&path, bytes).unwrap();
            let entries = VerifiedEntries::load(pool.path(), SETTLE, NOW);
            assert!(entries.record_ignored);
            assert_eq!(entries.known(&digest(1), &identity(1)), None);
            save(pool.path(), 1..2, NOW);
            assert!(recorded(pool.path(), 1, &identity(1), NOW));
        }
    }

    #[test]
    fn a_linked_or_shared_record_is_ignored() {
        let pool = tempfile::tempdir().unwrap();
        let path = pool.path().join(RECORD_FILE);
        save(pool.path(), 1..2, NOW);
        let elsewhere = pool.path().join("elsewhere");
        fs::hard_link(&path, &elsewhere).unwrap();
        assert!(!recorded(pool.path(), 1, &identity(1), NOW), "hard link");
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert!(!recorded(pool.path(), 1, &identity(1), NOW), "symlink");
        fs::remove_file(&path).unwrap();
        fs::rename(&elsewhere, &path).unwrap();
        assert!(recorded(pool.path(), 1, &identity(1), NOW));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o620)).unwrap();
        assert!(!recorded(pool.path(), 1, &identity(1), NOW), "writable");
        assert!(
            !VerifiedEntries::load(&pool.path().join("absent"), SETTLE, NOW).record_ignored,
            "no record is not an ignored one"
        );
    }

    /// Two passes loaded the same record: the later save keeps what the
    /// earlier one learned, and drops what it rejected itself.
    #[test]
    fn concurrent_passes_merge_and_a_rejection_is_not_kept() {
        let pool = tempfile::tempdir().unwrap();
        save(pool.path(), 1..2, NOW);
        let mut first = VerifiedEntries::load(pool.path(), SETTLE, NOW);
        let mut second = VerifiedEntries::load(pool.path(), SETTLE, NOW);
        first.verified(&digest(2), identity(2), NOW);
        second.verified(&digest(3), identity(3), NOW);
        second.rejected(&digest(1));
        assert_eq!(second.known(&digest(1), &identity(1)), None);
        first
            .save(pool.path(), &pool.path().join("first"), NOW)
            .unwrap();
        second
            .save(pool.path(), &pool.path().join("second"), NOW)
            .unwrap();
        assert!(!recorded(pool.path(), 1, &identity(1), NOW));
        assert!(recorded(pool.path(), 2, &identity(2), NOW));
        assert!(recorded(pool.path(), 3, &identity(3), NOW));
    }

    /// Writers racing on the same pool always leave one whole record whose
    /// entries carry the identity they were verified with.
    #[test]
    fn racing_writers_leave_a_whole_record() {
        let pool = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for writer in 0..8_usize {
                let pool = pool.path();
                scope.spawn(move || {
                    for round in 0..10 {
                        let mut entries = VerifiedEntries::load(pool, SETTLE, NOW);
                        assert!(!entries.record_ignored, "a torn record was seen");
                        entries.verified(&digest(writer), identity(writer as u64), NOW);
                        let temporary = pool.join(format!("temporary-{writer}-{round}"));
                        entries.save(pool, &temporary, NOW).unwrap();
                    }
                });
            }
        });
        let entries = VerifiedEntries::load(pool.path(), SETTLE, NOW);
        assert!(!entries.recorded.is_empty());
        for (name, verified) in &entries.recorded {
            let writer = usize::from_str_radix(name, 16).unwrap();
            assert_eq!(verified.identity, identity(writer as u64));
        }
    }

    #[test]
    fn the_record_is_bounded_and_readable_at_its_bound() {
        let pool = tempfile::tempdir().unwrap();
        save(pool.path(), 0..MAX_RECORDED_ENTRIES, NOW);
        save(
            pool.path(),
            MAX_RECORDED_ENTRIES..MAX_RECORDED_ENTRIES + 10,
            NOW + 1,
        );
        let entries = VerifiedEntries::load(pool.path(), SETTLE, NOW + 1);
        assert!(!entries.record_ignored);
        assert_eq!(entries.recorded.len(), MAX_RECORDED_ENTRIES);
        assert!(
            entries
                .recorded
                .contains_key(&digest(MAX_RECORDED_ENTRIES + 9))
        );
    }
}
