//! One host-wide watcher that lets a command reuse a worktree's changed-path
//! answer while nothing in that worktree changed.
//!
//! A lease scan asks Git two questions per active session (`git diff
//! --name-only <base>` and `git ls-files --others`). On a 373-session
//! repository that was about 330 ms per active session on every `agents`
//! call (2026-11-12). Most of those worktrees had not changed since the
//! previous command asked.
//!
//! The watcher never answers the question itself. Git stays the only source
//! of the answer, so ignore rules, renames and staged changes keep Git's
//! meaning. The watcher publishes one counter per worktree that moves when the
//! file system reports a change under it that could outdate a stored answer
//! (see the claim rule below). A command that scans a
//! worktree stores Git's answer beside the counter it read *before* the scan;
//! a later command reuses the answer only while the counter, the index file
//! and the baseline commit are all the same and the watcher has been alive
//! throughout.
//!
//! - **One watcher per host user.** It runs inside the host supervisor
//!   (`aethyme env supervise`), holds `watch.lock` exclusively for its whole
//!   life, and watches every worktree commands have asked about.
//! - **Fail open.** No watcher, an unreadable file, a lost file-system event
//!   or any mismatch means "unknown", and the command asks Git as before.
//!   File events on an external volume do get dropped under bursts (8 of 5,000
//!   in one probe on 2026-11-12), but the system flags the loss, and a flagged
//!   loss counts as a change to every worktree.
//! - **No event filtering.** Every non-read event counts, including build
//!   output and the Git directory: ignore rules (`.gitignore`, `info/exclude`,
//!   `core.excludesFile`) decide what `ls-files --others --exclude-standard`
//!   reports, and the watcher does not re-derive them. The claim rule makes
//!   those events cheap instead.
//! - **Bounded staleness, read-only commands only.** A counter is published up
//!   to [`COALESCE`] after the file system reports the change, so a reused
//!   answer can be that much older than a fresh one. Only commands that call
//!   [`allow_reuse_for_this_command`] reuse answers; anything that decides
//!   whether work may land keeps asking Git.
//! - **Publish only what a reader could act on.** A command that is about to
//!   ask Git first writes a *claim*: the counter it observed. The watcher moves
//!   a published counter only when a change arrives and a claim names that
//!   counter. Without a claim no answer can be stored against the counter, and
//!   a command that claims it later runs Git after the change, so its answer
//!   already includes the change. A build writing thousands of files while
//!   nobody asks therefore costs no writes at all (2026-11-16: the busiest
//!   machine dirtied 34 GB in under an hour republishing counters).
//! - **Bounded loop.** After the first event of a burst the watcher keeps
//!   draining for [`COALESCE`] before it acts, so it runs at most
//!   `1 / COALESCE` rounds a second and writes each counter at most once per
//!   round.
//! - **Status.** The watcher writes `status.json` in the watch directory at
//!   most once every [`STATUS_EVERY`]: events received and coalesced, rounds,
//!   reviews, counters written and skipped by reason, and the watched
//!   worktrees.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::git::{GitError, GitRepo};

/// Directory under the host state directory. The suffix is the on-disk
/// protocol version: a different layout must use a different directory, so
/// two binaries never read each other's files.
const DIRECTORY: &str = "worktree-watch-v1";
#[cfg_attr(not(unix), allow(dead_code))]
const LOCK: &str = "watch.lock";
/// Held by the watcher only once its epoch is published; see `run`.
#[cfg_attr(not(unix), allow(dead_code))]
const READY: &str = "ready.lock";
#[cfg_attr(not(unix), allow(dead_code))]
const EPOCH: &str = "epoch";
#[cfg_attr(not(unix), allow(dead_code))]
const WANTED: &str = "wanted";
#[cfg_attr(not(unix), allow(dead_code))]
const SEEN: &str = "seen";
#[cfg_attr(not(unix), allow(dead_code))]
const ANSWERS: &str = "answers";
/// `claims/<key>.<epoch>.<counter>`: an empty file saying a command observed
/// that counter and is about to ask Git.
#[cfg_attr(not(unix), allow(dead_code))]
const CLAIMS: &str = "claims";
#[cfg_attr(not(unix), allow(dead_code))]
const STATUS: &str = "status.json";

/// How long the watcher waits for an event before it retries counters it
/// could not write.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) const FLUSH_EVERY: Duration = Duration::from_millis(100);
/// After the first event of a burst, how long the watcher keeps draining
/// events before it acts. It bounds the watcher to `1 / COALESCE` rounds a
/// second, and so every counter to that many writes a second.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) const COALESCE: Duration = Duration::from_millis(50);
/// How often the watcher rewrites `status.json`.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) const STATUS_EVERY: Duration = Duration::from_secs(60);
/// A temporary file this old was left by a process that died mid-write.
#[cfg_attr(not(unix), allow(dead_code))]
const STALE_TEMPORARY: Duration = Duration::from_secs(60);
/// A stored answer older than this is asked again, whatever the counter says.
#[cfg_attr(not(unix), allow(dead_code))]
const ANSWER_MAX_AGE: Duration = Duration::from_secs(600);
/// A worktree nobody asked about for this long is no longer watched.
#[cfg_attr(not(unix), allow(dead_code))]
const WANTED_KEEP: Duration = Duration::from_secs(1800);
/// The supervisor stays up while some worktree was asked about this recently.
#[cfg_attr(not(unix), allow(dead_code))]
const WANTED_ACTIVE: Duration = Duration::from_secs(600);
/// A command refreshes its interest in a worktree at most this often.
#[cfg_attr(not(unix), allow(dead_code))]
const WANTED_REFRESH: Duration = Duration::from_secs(60);

/// Set `AETHYME_WORKTREE_WATCH=off` to always ask Git.
const SWITCH_ENV: &str = "AETHYME_WORKTREE_WATCH";

static REUSE_ALLOWED: AtomicBool = AtomicBool::new(false);

/// Let this process reuse changed-path answers. A process-wide switch, not a
/// thread-local: lease scans run on worker threads, which do not inherit
/// thread-locals. Call it only from a command that reports and never decides.
pub(crate) fn allow_reuse_for_this_command() {
    REUSE_ALLOWED.store(true, Ordering::Relaxed);
}

/// The watch directory for this host user, if host state is available.
pub(crate) fn directory() -> Option<PathBuf> {
    crate::host_state::default_host_state_dir().map(|dir| dir.join(DIRECTORY))
}

/// `checkout.changed_files(base)`, reusing the previous answer when the
/// watcher shows the worktree unchanged and this command allowed reuse.
pub(crate) fn changed_files(checkout: &GitRepo, base: &str) -> Result<Vec<String>, GitError> {
    let switched_off = std::env::var_os(SWITCH_ENV).is_some_and(|value| value == "off");
    if switched_off || !REUSE_ALLOWED.load(Ordering::Relaxed) {
        return checkout.changed_files(base);
    }
    let Some(directory) = directory() else {
        return checkout.changed_files(base);
    };
    let (files, source) = changed_files_in(&directory, checkout, base)?;
    if source == Source::Git && !crate::host_state::host_state_dir_is_explicit() {
        imp::request_supervisor();
    }
    Ok(files)
}

/// Where an answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) enum Source {
    /// Git was asked.
    Git,
    /// The stored answer was reused.
    Reused,
}

/// [`changed_files`] against an explicit watch directory.
pub(crate) fn changed_files_in(
    directory: &Path,
    checkout: &GitRepo,
    base: &str,
) -> Result<(Vec<String>, Source), GitError> {
    let before = imp::observe(directory, checkout.root(), base);
    if let Some(observed) = &before
        && let Some(files) = imp::read_answer(directory, observed)
    {
        crate::phase_telemetry::add("changed_files_reused", Duration::ZERO);
        return Ok((files, Source::Reused));
    }
    // The claim must land before Git runs: the watcher moves the counter on
    // a later change only if it finds the claim, and a change it saw before
    // the claim landed is one Git sees too. No claim, no stored answer.
    let claimed = before
        .as_ref()
        .is_some_and(|observed| imp::claim(directory, observed));
    let files = {
        let _span = crate::phase_telemetry::span("changed_files_asked_git");
        checkout.changed_files(base)?
    };
    if let Some(observed) = before
        && claimed
    {
        imp::write_answer(directory, &observed, &files);
    }
    Ok((files, Source::Git))
}

#[cfg(not(unix))]
mod imp {
    use std::path::Path;

    pub(super) struct Observed;

    pub(super) fn observe(_: &Path, _: &Path, _: &str) -> Option<Observed> {
        None
    }
    pub(super) fn read_answer(_: &Path, _: &Observed) -> Option<Vec<String>> {
        None
    }
    pub(super) fn claim(_: &Path, _: &Observed) -> bool {
        false
    }
    pub(super) fn write_answer(_: &Path, _: &Observed, _: &[String]) {}
    pub(super) fn request_supervisor() {}
}

#[cfg(unix)]
mod imp {
    use std::collections::HashMap;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::{Duration, Instant, SystemTime};

    use notify::{RecursiveMode, Watcher};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};

    use super::{
        ANSWER_MAX_AGE, ANSWERS, CLAIMS, COALESCE, EPOCH, FLUSH_EVERY, LOCK, READY, SEEN,
        STALE_TEMPORARY, STATUS, STATUS_EVERY, WANTED, WANTED_ACTIVE, WANTED_KEEP, WANTED_REFRESH,
    };
    use crate::environments::supervisor::{SupervisorWork, TickOutcome};
    use crate::file_lock::{ExclusiveFileLock, SharedFileLock, open_lock_file};

    /// What a command read about a worktree before it decided to scan or reuse.
    pub(crate) struct Observed {
        key: String,
        /// `<epoch>:<counter>` as the watcher last published it.
        token: String,
        base: String,
        index: IndexStamp,
    }

    #[cfg(test)]
    impl Observed {
        pub(crate) fn token(&self) -> &str {
            &self.token
        }
    }

    /// The identity of a worktree's index file. `git add`, `git rm --cached`
    /// and `git mv` change the answer without touching the worktree's files,
    /// and each of them rewrites the index.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct IndexStamp {
        modified_secs: u64,
        modified_nanos: u32,
        len: u64,
    }

    #[derive(Serialize, Deserialize)]
    struct Answer {
        token: String,
        base: String,
        index: IndexStamp,
        recorded_at_ms: u64,
        files: Vec<String>,
    }

    /// Drop the result of writing or removing one of this module's files.
    ///
    /// Every file here is a hint, never broker state: a missing or stale
    /// request, counter or answer makes the next command ask Git, and a
    /// counter that could not be published stays marked and is retried.
    fn hint<T, E>(_result: Result<T, E>) {}

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or(0)
    }

    /// A stable file name for a canonical worktree path.
    pub(crate) fn key_for(canonical: &Path) -> String {
        let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
        digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// The index file of the checkout at `worktree`: `.git/index`, or the
    /// index of the linked worktree its `.git` file names.
    fn index_path(worktree: &Path) -> Option<PathBuf> {
        let dot_git = worktree.join(".git");
        let metadata = fs::symlink_metadata(&dot_git).ok()?;
        if metadata.is_dir() {
            return Some(dot_git.join("index"));
        }
        if !metadata.is_file() || metadata.len() > 4096 {
            return None;
        }
        let text = fs::read_to_string(&dot_git).ok()?;
        let named = text.strip_prefix("gitdir:")?.trim();
        let git_dir = Path::new(named);
        let git_dir = if git_dir.is_absolute() {
            git_dir.to_path_buf()
        } else {
            worktree.join(git_dir)
        };
        Some(git_dir.join("index"))
    }

    fn index_stamp(worktree: &Path) -> Option<IndexStamp> {
        let metadata = fs::metadata(index_path(worktree)?).ok()?;
        let modified = metadata
            .modified()
            .ok()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()?;
        Some(IndexStamp {
            modified_secs: modified.as_secs(),
            modified_nanos: modified.subsec_nanos(),
            len: metadata.len(),
        })
    }

    /// True while a watcher holds `ready.lock`, which it takes only after it
    /// has replaced the previous watcher's epoch and counters. The probe takes
    /// the lock shared, so two probes never mistake each other for a watcher,
    /// and a watcher that died released it with its last descriptor.
    fn watcher_is_alive(directory: &Path) -> bool {
        let Ok(file) = fs::File::open(directory.join(READY)) else {
            return false;
        };
        matches!(SharedFileLock::try_acquire(file), Ok(None))
    }

    fn is_commit_id(base: &str) -> bool {
        matches!(base.len(), 40 | 64) && base.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    /// Say this worktree is of interest, so the watcher starts or keeps
    /// watching it. The file holds the path; its modification time is the
    /// last time anyone asked.
    fn request_watch(directory: &Path, key: &str, canonical: &Path) {
        let path = directory.join(WANTED).join(key);
        let fresh = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age < WANTED_REFRESH);
        if fresh {
            return;
        }
        let Some(text) = canonical.to_str() else {
            return;
        };
        hint(fs::create_dir_all(directory.join(WANTED)));
        hint(write_atomically(&path, text.as_bytes()));
    }

    pub(super) fn observe(directory: &Path, worktree: &Path, base: &str) -> Option<Observed> {
        // A symbolic baseline such as `HEAD` names a different commit later.
        if !is_commit_id(base) {
            return None;
        }
        let canonical = fs::canonicalize(worktree).ok()?;
        let key = key_for(&canonical);
        request_watch(directory, &key, &canonical);
        // Order matters: the lock is checked first and the counter read
        // after, so a counter is never trusted from before a watcher's death.
        if !watcher_is_alive(directory) {
            return None;
        }
        let epoch = fs::read_to_string(directory.join(EPOCH)).ok()?;
        let seen = fs::read_to_string(directory.join(SEEN).join(&key)).ok()?;
        let (seen_epoch, counter) = seen.trim().split_once(':')?;
        if epoch.trim().is_empty() || seen_epoch != epoch.trim() {
            return None;
        }
        counter.parse::<u64>().ok()?;
        Some(Observed {
            key,
            token: seen.trim().to_string(),
            base: base.to_string(),
            index: index_stamp(&canonical)?,
        })
    }

    pub(super) fn read_answer(directory: &Path, observed: &Observed) -> Option<Vec<String>> {
        let text = fs::read(directory.join(ANSWERS).join(&observed.key)).ok()?;
        let answer: Answer = serde_json::from_slice(&text).ok()?;
        let age = Duration::from_millis(now_ms().checked_sub(answer.recorded_at_ms)?);
        let same = answer.token == observed.token
            && answer.base == observed.base
            && answer.index == observed.index
            && age < ANSWER_MAX_AGE;
        same.then_some(answer.files)
    }

    /// The claim file for one published counter of one worktree.
    fn claim_name(key: &str, token: &str) -> String {
        format!("{key}.{}", token.replace(':', "."))
    }

    pub(crate) fn claim_path(directory: &Path, observed: &Observed) -> PathBuf {
        directory
            .join(CLAIMS)
            .join(claim_name(&observed.key, &observed.token))
    }

    /// Say "an answer may be stored against this counter" before asking Git.
    /// The watcher moves a counter on a change only when the claim for it
    /// exists; see [`Core::flush`]. False when the claim could not be made,
    /// and the caller must then not store its answer.
    ///
    /// One file per counter, created and never rewritten: two commands that
    /// observed the same counter share it, and a slow command claiming an
    /// older counter cannot hide a newer command's claim. Whoever created the
    /// file, it existed before this command asks Git.
    pub(crate) fn claim(directory: &Path, observed: &Observed) -> bool {
        let path = claim_path(directory, observed);
        let create = || {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
        };
        let created = create().or_else(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                fs::create_dir_all(directory.join(CLAIMS))?;
                create()
            } else {
                Err(error)
            }
        });
        match created {
            Ok(_) => true,
            Err(error) => error.kind() == io::ErrorKind::AlreadyExists,
        }
    }

    /// Store Git's answer beside what was observed *before* Git ran. A change
    /// during the scan moves the counter past the stored one (the command
    /// claimed it first), so the answer is then never reused.
    pub(crate) fn write_answer(directory: &Path, observed: &Observed, files: &[String]) {
        let answer = Answer {
            token: observed.token.clone(),
            base: observed.base.clone(),
            index: observed.index.clone(),
            recorded_at_ms: now_ms(),
            files: files.to_vec(),
        };
        let Ok(bytes) = serde_json::to_vec(&answer) else {
            return;
        };
        hint(fs::create_dir_all(directory.join(ANSWERS)));
        hint(write_atomically(
            &directory.join(ANSWERS).join(&observed.key),
            &bytes,
        ));
    }

    fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("unnamed watch file"))?;
        let temporary = path.with_file_name(format!(
            ".{name}.{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&temporary, bytes)?;
        fs::rename(&temporary, path).inspect_err(|_| {
            hint(fs::remove_file(&temporary));
        })
    }

    /// Start the host supervisor in the background, at most once per command.
    /// The command does not wait for it: this command asked Git already, and
    /// the next one finds the watcher.
    pub(super) fn request_supervisor() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let Some(root) = crate::environments::logs::environments_directory() else {
                return;
            };
            let Ok(executable) = std::env::current_exe() else {
                return;
            };
            std::thread::spawn(move || {
                hint(crate::environments::supervisor::ensure_running(
                    &root,
                    &executable,
                ));
            });
        });
    }

    /// `.<name>.<pid>.<sequence>`, as [`write_atomically`] names its files.
    fn is_temporary(name: &str) -> bool {
        let Some(rest) = name.strip_prefix('.') else {
            return false;
        };
        let mut parts = rest.rsplitn(3, '.');
        let numeric = |part: Option<&str>| {
            part.is_some_and(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        };
        numeric(parts.next())
            && numeric(parts.next())
            && parts.next().is_some_and(|n| !n.is_empty())
    }

    /// Remove temporaries a process left behind when it died between writing
    /// and renaming. Commands write temporaries here too, so only old ones go.
    fn remove_stale_temporaries(directory: &Path) {
        for folder in [
            directory.to_path_buf(),
            directory.join(WANTED),
            directory.join(ANSWERS),
        ] {
            let Ok(entries) = fs::read_dir(&folder) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                if !name.to_str().is_some_and(is_temporary) {
                    continue;
                }
                let old = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age >= STALE_TEMPORARY);
                if old {
                    hint(fs::remove_file(entry.path()));
                }
            }
        }
    }

    /// What the watcher did since it started. Kept in memory and written to
    /// `status.json` at most once every [`STATUS_EVERY`], so it adds no
    /// write load of its own.
    #[derive(Default)]
    pub(crate) struct Stats {
        pub(crate) events_received: AtomicU64,
        /// Events that arrived while a round was already draining a burst.
        pub(crate) events_coalesced: AtomicU64,
        /// Reads, which never move a counter.
        pub(crate) events_ignored: AtomicU64,
        /// Errors, flagged losses and watch-set changes: every worktree changed.
        pub(crate) rescans: AtomicU64,
        pub(crate) rounds: AtomicU64,
        pub(crate) reviews: AtomicU64,
        /// A newly watched worktree's first counter.
        pub(crate) published_initial: AtomicU64,
        /// A change arrived and a command had claimed the published counter.
        pub(crate) published_claimed: AtomicU64,
        /// A change arrived and nobody had claimed the published counter.
        pub(crate) skipped_unclaimed: AtomicU64,
        pub(crate) publish_failures: AtomicU64,
        pub(crate) watched: AtomicU64,
    }

    impl Stats {
        fn bump(counter: &AtomicU64) {
            counter.fetch_add(1, Ordering::Relaxed);
        }

        fn to_json(&self, epoch: &str) -> serde_json::Value {
            let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
            serde_json::json!({
                "epoch": epoch,
                "updated_at_ms": now_ms(),
                "events_received": read(&self.events_received),
                "events_coalesced": read(&self.events_coalesced),
                "events_ignored": read(&self.events_ignored),
                "rescans": read(&self.rescans),
                "rounds": read(&self.rounds),
                "reviews": read(&self.reviews),
                "published_initial": read(&self.published_initial),
                "published_claimed": read(&self.published_claimed),
                "skipped_unclaimed": read(&self.skipped_unclaimed),
                "publish_failures": read(&self.publish_failures),
                "watched": read(&self.watched),
            })
        }
    }

    struct Watched {
        path: PathBuf,
        /// The counter on disk in `seen/<key>`, once one was written.
        published: Option<u64>,
        /// A change arrived since the watcher last decided about this worktree.
        changed: bool,
    }

    /// Everything the watcher decides, without the file-system watcher, so
    /// tests can drive each ordering of changes, claims and answers.
    pub(crate) struct Core {
        epoch: String,
        /// The last counter handed out. Shared by every worktree, so a
        /// counter is never published twice in one epoch, not even for a
        /// worktree that was forgotten and requested again.
        sequence: u64,
        watching: HashMap<String, Watched>,
        pub(crate) stats: Arc<Stats>,
    }

    impl Core {
        pub(crate) fn new(epoch: String, stats: Arc<Stats>) -> Self {
            Self {
                epoch,
                sequence: 0,
                watching: HashMap::new(),
                stats,
            }
        }

        pub(crate) fn watch(&mut self, key: String, path: PathBuf) {
            self.watching.insert(
                key,
                Watched {
                    path,
                    published: None,
                    changed: false,
                },
            );
            self.stats
                .watched
                .store(self.watching.len() as u64, Ordering::Relaxed);
        }

        fn forget(&mut self, key: &str) -> Option<PathBuf> {
            let removed = self.watching.remove(key).map(|watched| watched.path);
            self.stats
                .watched
                .store(self.watching.len() as u64, Ordering::Relaxed);
            removed
        }

        pub(crate) fn change_all(&mut self) {
            Stats::bump(&self.stats.rescans);
            for watched in self.watching.values_mut() {
                watched.changed = true;
            }
        }

        pub(crate) fn record(&mut self, event: notify::Result<notify::Event>) {
            Stats::bump(&self.stats.events_received);
            let event = match event {
                Ok(event) if !event.need_rescan() => event,
                // An error or a flagged loss: nothing is known about any worktree.
                _ => {
                    self.change_all();
                    return;
                }
            };
            // Reads are reported on Linux (inotify opens and closes), and a lease
            // scan itself reads the worktree: only changes move a counter.
            if matches!(event.kind, notify::EventKind::Access(_)) {
                Stats::bump(&self.stats.events_ignored);
                return;
            }
            for path in &event.paths {
                for watched in self.watching.values_mut() {
                    if path.starts_with(&watched.path) {
                        watched.changed = true;
                    }
                }
            }
        }

        /// Publish what a reader could act on, and nothing else.
        ///
        /// - A worktree with no counter on disk gets its first one.
        /// - A changed worktree gets a new counter only if the claim for the
        ///   published one exists. Every stored answer was claimed first
        ///   (see [`super::changed_files_in`]), so without that claim no
        ///   stored answer names the published counter and none can be
        ///   reused. A command that claims it after this check runs Git after
        ///   the change arrived here, so its answer includes the change: the
        ///   change is settled either way and the worktree is marked clean.
        /// - A claim that cannot be read counts as a claim.
        pub(crate) fn flush(&mut self, directory: &Path) {
            for (key, watched) in &mut self.watching {
                let publish = match watched.published {
                    None => Some(&self.stats.published_initial),
                    Some(_) if !watched.changed => None,
                    Some(published) => {
                        let name = claim_name(key, &format!("{}:{published}", self.epoch));
                        let claimed = match fs::symlink_metadata(directory.join(CLAIMS).join(name))
                        {
                            Ok(_) => true,
                            Err(error) => error.kind() != io::ErrorKind::NotFound,
                        };
                        if claimed {
                            Some(&self.stats.published_claimed)
                        } else {
                            Stats::bump(&self.stats.skipped_unclaimed);
                            watched.changed = false;
                            None
                        }
                    }
                };
                let Some(reason) = publish else {
                    continue;
                };
                let next = self.sequence + 1;
                let line = format!("{}:{next}", self.epoch);
                if write_atomically(&directory.join(SEEN).join(key), line.as_bytes()).is_ok() {
                    self.sequence = next;
                    // The superseded counter's claim can go: no answer
                    // stored against that counter is reusable any more.
                    if let Some(old) = watched.published {
                        let old = claim_name(key, &format!("{}:{old}", self.epoch));
                        hint(fs::remove_file(directory.join(CLAIMS).join(old)));
                    }
                    watched.published = Some(next);
                    watched.changed = false;
                    Stats::bump(reason);
                } else {
                    // Kept marked: retried on the next round.
                    Stats::bump(&self.stats.publish_failures);
                }
            }
        }

        /// Remove claims that name no currently published counter: those of
        /// forgotten worktrees, and those a slow command made for a counter
        /// that was already superseded. The claim for a published counter is
        /// never removed, so this can never hide a claim [`Core::flush`] needs.
        pub(crate) fn sweep_claims(&self, directory: &Path) {
            let Ok(entries) = fs::read_dir(directory.join(CLAIMS)) else {
                return;
            };
            for entry in entries.flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                let key = name.split('.').next().unwrap_or_default();
                let current = self
                    .watching
                    .get(key)
                    .and_then(|watched| watched.published)
                    .map(|published| claim_name(key, &format!("{}:{published}", self.epoch)));
                if current.as_deref() != Some(name.as_str()) {
                    hint(fs::remove_file(entry.path()));
                }
            }
        }
    }

    /// The watcher as one unit of supervisor work. It owns a thread, because
    /// counters must be published far more often than the supervisor ticks.
    pub struct WorktreeWatchService {
        directory: PathBuf,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl WorktreeWatchService {
        pub fn new(directory: PathBuf) -> Self {
            Self {
                directory,
                stop: Arc::new(AtomicBool::new(false)),
                thread: None,
            }
        }

        fn ensure_thread(&mut self) {
            if self
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
            {
                return;
            }
            let directory = self.directory.clone();
            let stop = Arc::clone(&self.stop);
            self.thread = std::thread::Builder::new()
                .name("worktree-watch".into())
                .spawn(move || {
                    if let Err(error) = run(&directory, &stop) {
                        eprintln!("worktree watch stopped: {error}");
                    }
                })
                .ok();
        }
    }

    impl Drop for WorktreeWatchService {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                hint(thread.join());
            }
        }
    }

    impl SupervisorWork for WorktreeWatchService {
        fn tick(&mut self) -> TickOutcome {
            let needed = wanted(&self.directory)
                .into_iter()
                .any(|(_, _, age)| age < WANTED_ACTIVE);
            if needed {
                self.ensure_thread();
            }
            TickOutcome {
                needs_supervisor: needed,
            }
        }
    }

    /// Every requested worktree: key, path and time since it was last asked about.
    fn wanted(directory: &Path) -> Vec<(String, PathBuf, Duration)> {
        let Ok(entries) = fs::read_dir(directory.join(WANTED)) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let key = entry.file_name().into_string().ok()?;
                if key.starts_with('.') {
                    return None;
                }
                let age = entry.metadata().ok()?.modified().ok()?.elapsed().ok()?;
                let path = PathBuf::from(fs::read_to_string(entry.path()).ok()?);
                Some((key, path, age))
            })
            .collect()
    }

    /// What a running watcher holds. Commands trust the directory only while
    /// `ready.lock` is held, and it is taken only after the new epoch is on
    /// disk.
    pub(crate) struct Started {
        _lock: ExclusiveFileLock,
        _ready: ExclusiveFileLock,
        pub(crate) epoch: String,
    }

    /// Take the watcher's locks and start a new epoch. `None` when another
    /// watcher holds the lock, or `stop` was set while waiting for it.
    pub(crate) fn start(directory: &Path, stop: &AtomicBool) -> io::Result<Option<Started>> {
        for name in [WANTED, SEEN, ANSWERS, CLAIMS] {
            fs::create_dir_all(directory.join(name))?;
        }
        crate::host_state::protect_host_state_path(directory, true)?;
        // Probes hold the lock shared for an instant, so retry briefly.
        let mut lock = None;
        for _ in 0..50 {
            lock = ExclusiveFileLock::try_acquire(open_lock_file(&directory.join(LOCK))?)?;
            if lock.is_some() || stop.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let Some(lock) = lock else {
            return Ok(None);
        };
        // A new epoch outdates every counter, claim and answer of earlier
        // watchers. Counters and claims are removed outright; only this
        // watcher writes counters, and a claim naming an old epoch is inert.
        for folder in [SEEN, CLAIMS] {
            for entry in fs::read_dir(directory.join(folder))?.flatten() {
                hint(fs::remove_file(entry.path()));
            }
        }
        remove_stale_temporaries(directory);
        let epoch = format!("{}-{}", std::process::id(), now_ms());
        write_atomically(&directory.join(EPOCH), epoch.as_bytes())?;
        // Only now may a command trust what it reads here. Between taking
        // `watch.lock` and this point the directory still held the previous
        // watcher's epoch and counters, and a command that believed a watcher
        // was alive would have reused an answer from before that watcher died.
        let mut ready = None;
        for _ in 0..250 {
            ready = ExclusiveFileLock::try_acquire(open_lock_file(&directory.join(READY))?)?;
            if ready.is_some() || stop.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let Some(ready) = ready else {
            return Ok(None);
        };
        Ok(Some(Started {
            _lock: lock,
            _ready: ready,
            epoch,
        }))
    }

    /// Watch until `stop` is set. Returns at once when another watcher holds
    /// the lock.
    pub(crate) fn run(directory: &Path, stop: &AtomicBool) -> io::Result<()> {
        run_with_stats(directory, stop, Arc::new(Stats::default()))
    }

    /// [`run`], counting into `stats`.
    pub(crate) fn run_with_stats(
        directory: &Path,
        stop: &AtomicBool,
        stats: Arc<Stats>,
    ) -> io::Result<()> {
        let Some(started) = start(directory, stop)? else {
            return Ok(());
        };
        let (sender, events) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            hint(sender.send(event));
        })
        .map_err(io::Error::other)?;
        let mut core = Core::new(started.epoch.clone(), stats);
        let mut next_review = Instant::now();
        let mut next_status = Instant::now();

        while !stop.load(Ordering::Relaxed) {
            if Instant::now() >= next_review {
                next_review = Instant::now() + Duration::from_secs(1);
                review(directory, &mut watcher, &mut core);
            }
            match events.recv_timeout(FLUSH_EVERY) {
                Ok(event) => {
                    core.record(event);
                    coalesce(&events, &mut core)?;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("file watcher closed its channel"));
                }
            }
            core.flush(directory);
            Stats::bump(&core.stats.rounds);
            if Instant::now() >= next_status {
                next_status = Instant::now() + STATUS_EVERY;
                core.sweep_claims(directory);
                write_status(directory, &core);
            }
        }
        Ok(())
    }

    type Events = mpsc::Receiver<notify::Result<notify::Event>>;

    /// Keep taking events for [`COALESCE`] after the first one of a round.
    /// A round therefore lasts at least that long while events keep coming,
    /// however many are queued: the loop never spins.
    fn coalesce(events: &Events, core: &mut Core) -> io::Result<()> {
        let deadline = Instant::now() + COALESCE;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            match events.recv_timeout(remaining) {
                Ok(event) => {
                    Stats::bump(&core.stats.events_coalesced);
                    core.record(event);
                }
                Err(RecvTimeoutError::Timeout) => return Ok(()),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("file watcher closed its channel"));
                }
            }
        }
    }

    fn write_status(directory: &Path, core: &Core) {
        if let Ok(bytes) = serde_json::to_vec_pretty(&core.stats.to_json(&core.epoch)) {
            hint(write_atomically(&directory.join(STATUS), &bytes));
        }
    }

    /// Start watching newly requested worktrees and stop watching forgotten ones.
    fn review(directory: &Path, watcher: &mut notify::RecommendedWatcher, core: &mut Core) {
        Stats::bump(&core.stats.reviews);
        let wanted = wanted(directory);
        let mut changed_watch_set = false;
        for (key, path, age) in &wanted {
            let keep = *age < WANTED_KEEP && path.is_dir();
            if keep && !core.watching.contains_key(key) {
                if watcher.watch(path, RecursiveMode::Recursive).is_ok() {
                    core.watch(key.clone(), path.clone());
                    changed_watch_set = true;
                }
            } else if !keep {
                if let Some(path) = core.forget(key) {
                    hint(watcher.unwatch(&path));
                    changed_watch_set = true;
                }
                // Its claims go with the next sweep.
                for folder in [SEEN, ANSWERS, WANTED] {
                    hint(fs::remove_file(directory.join(folder).join(key)));
                }
            }
        }
        // Changing the watch set can restart the underlying event stream, and
        // events in that gap are lost: treat every worktree as changed.
        if changed_watch_set {
            core.change_all();
        }
    }
}

#[cfg(unix)]
pub use imp::WorktreeWatchService;

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::Arc;
    use std::time::Instant;

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .expect("git runs");
        assert!(status.status.success(), "git {args:?} failed");
    }

    struct Fixture {
        _holder: tempfile::TempDir,
        directory: PathBuf,
        checkout: GitRepo,
        base: String,
        stop: Arc<AtomicBool>,
        watcher: Option<std::thread::JoinHandle<()>>,
        stats: Arc<imp::Stats>,
    }

    impl Fixture {
        fn new() -> Self {
            let holder = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(holder.path()).unwrap();
            let repository = root.join("repository");
            std::fs::create_dir_all(&repository).unwrap();
            git(&repository, &["init", "-q", "-b", "main"]);
            std::fs::write(repository.join("tracked.txt"), "one\n").unwrap();
            git(&repository, &["add", "tracked.txt"]);
            git(&repository, &["commit", "-q", "-m", "first"]);
            let head = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repository)
                .output()
                .unwrap();
            Self {
                directory: root.join("watch"),
                checkout: GitRepo::discover(&repository).unwrap(),
                base: String::from_utf8(head.stdout).unwrap().trim().to_string(),
                stop: Arc::new(AtomicBool::new(false)),
                watcher: None,
                stats: Arc::new(imp::Stats::default()),
                _holder: holder,
            }
        }

        fn start_watcher(&mut self) {
            let directory = self.directory.clone();
            let stop = Arc::clone(&self.stop);
            let stats = Arc::clone(&self.stats);
            self.stop.store(false, Ordering::Relaxed);
            self.watcher = Some(std::thread::spawn(move || {
                imp::run_with_stats(&directory, &stop, stats).unwrap();
            }));
        }

        fn stop_watcher(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(watcher) = self.watcher.take() {
                watcher.join().unwrap();
            }
        }

        fn ask(&self) -> (Vec<String>, Source) {
            changed_files_in(&self.directory, &self.checkout, &self.base).unwrap()
        }

        /// What the watcher has published, for a failure message.
        fn published(&self) -> Vec<String> {
            let mut lines = vec![format!(
                "epoch={:?}",
                std::fs::read_to_string(self.directory.join(EPOCH)).ok()
            )];
            for name in [WANTED, SEEN, ANSWERS] {
                let entries = std::fs::read_dir(self.directory.join(name))
                    .map(|entries| entries.flatten().collect::<Vec<_>>())
                    .unwrap_or_default();
                for entry in entries {
                    lines.push(format!(
                        "{name}/{}={:?}",
                        entry.file_name().to_string_lossy(),
                        std::fs::read_to_string(entry.path())
                            .ok()
                            .map(|text| text.chars().take(120).collect::<String>())
                    ));
                }
            }
            lines
        }

        /// Ask until the answer comes from `source`, for at most ten seconds.
        fn ask_until(&self, source: Source) -> Vec<String> {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let (files, came_from) = self.ask();
                if came_from == source {
                    return files;
                }
                assert!(
                    Instant::now() < deadline,
                    "no answer from {source:?} within ten seconds; published counters: {:?}",
                    self.published()
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(watcher) = self.watcher.take() {
                let _ = watcher.join();
            }
        }
    }

    #[test]
    fn without_a_watcher_every_answer_comes_from_git() {
        let fixture = Fixture::new();
        for _ in 0..3 {
            let (files, source) = fixture.ask();
            assert_eq!(source, Source::Git);
            assert!(files.is_empty());
        }
    }

    #[test]
    fn an_unchanged_worktree_is_answered_without_git_and_a_change_asks_again() {
        let mut fixture = Fixture::new();
        fixture.start_watcher();
        // The first questions register the worktree; once it is watched, an
        // answer is stored and then reused.
        assert!(fixture.ask_until(Source::Reused).is_empty());

        // A new untracked file must be reported, never hidden by the old answer.
        std::fs::write(fixture.checkout.root().join("new.txt"), "x\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (files, _) = fixture.ask();
            if files == ["new.txt"] {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the new file was never reported: {files:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(fixture.ask_until(Source::Reused), ["new.txt"]);

        // An edit to a tracked file is reported too.
        std::fs::write(fixture.checkout.root().join("tracked.txt"), "two\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (files, _) = fixture.ask();
            if files == ["new.txt", "tracked.txt"] {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the edit was never reported: {files:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn an_index_only_change_is_never_answered_from_the_old_answer() {
        let mut fixture = Fixture::new();
        fixture.start_watcher();
        assert!(fixture.ask_until(Source::Reused).is_empty());
        // Removing a file from the index leaves the worktree's files alone.
        git(
            fixture.checkout.root(),
            &["rm", "-q", "--cached", "tracked.txt"],
        );
        let (files, source) = fixture.ask();
        assert_eq!(source, Source::Git);
        assert_eq!(files, ["tracked.txt"]);
    }

    #[test]
    fn answers_stored_under_a_dead_watcher_are_not_reused() {
        let mut fixture = Fixture::new();
        fixture.start_watcher();
        assert!(fixture.ask_until(Source::Reused).is_empty());
        fixture.stop_watcher();
        // Nothing reports changes now, so the stored answer cannot be trusted.
        std::fs::write(fixture.checkout.root().join("unseen.txt"), "x\n").unwrap();
        let (files, source) = fixture.ask();
        assert_eq!(source, Source::Git);
        assert_eq!(files, ["unseen.txt"]);
        // A new watcher starts a new epoch: the answer stored before it is
        // not reused either, until Git has been asked under the new watcher.
        fixture.start_watcher();
        assert_eq!(fixture.ask_until(Source::Reused), ["unseen.txt"]);
    }

    #[test]
    fn a_symbolic_baseline_is_never_stored() {
        let mut fixture = Fixture::new();
        fixture.start_watcher();
        assert!(fixture.ask_until(Source::Reused).is_empty());
        for _ in 0..3 {
            let (_, source) =
                changed_files_in(&fixture.directory, &fixture.checkout, "HEAD").unwrap();
            assert_eq!(source, Source::Git);
        }
    }

    fn load(counter: &std::sync::atomic::AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    #[test]
    fn an_event_storm_writes_a_bounded_number_of_counters_and_still_asks_git() {
        let mut fixture = Fixture::new();
        fixture.start_watcher();
        assert!(fixture.ask_until(Source::Reused).is_empty());
        let stats = Arc::clone(&fixture.stats);
        let published = || load(&stats.published_initial) + load(&stats.published_claimed);
        let (published_before, rounds_before) = (published(), load(&stats.rounds));
        let started = Instant::now();

        // A build rewriting many files as fast as it can, with nobody asking.
        let root = fixture.checkout.root().to_path_buf();
        let mut writes = 0_u64;
        while started.elapsed() < Duration::from_secs(1) && writes < 20_000 {
            std::fs::write(
                root.join(format!("storm-{}.txt", writes % 200)),
                writes.to_string(),
            )
            .unwrap();
            writes += 1;
        }
        let elapsed = started.elapsed();
        let (published_during, rounds_during) = (
            published() - published_before,
            load(&stats.rounds) - rounds_before,
        );
        assert!(writes >= 1_000, "the storm wrote only {writes} files");
        // One counter for the first change after the stored answer's claim;
        // nobody claimed the new counter, so the rest of the storm is free.
        assert!(
            published_during <= 3,
            "{published_during} counters written for {writes} file writes"
        );
        // Every round lasts at least COALESCE while events keep coming.
        let ceiling = (elapsed.as_millis() / COALESCE.as_millis()) as u64 + 3;
        assert!(
            rounds_during <= ceiling,
            "{rounds_during} rounds in {elapsed:?}, ceiling {ceiling}"
        );

        // The answer stored before the storm is never reused after it.
        let (files, source) = fixture.ask();
        assert_eq!(source, Source::Git, "published: {:?}", fixture.published());
        assert_eq!(files.len(), 200, "{files:?}");
        assert!(files.iter().all(|file| file.starts_with("storm-")));
    }

    /// A watcher core driven by hand, with no file-system watcher and no
    /// thread, so every ordering of change, claim, answer and flush is exact.
    struct Driven {
        fixture: Fixture,
        /// Held for its locks.
        _started: Option<imp::Started>,
        core: imp::Core,
        key: String,
        root: PathBuf,
    }

    impl Driven {
        fn new() -> Self {
            let fixture = Fixture::new();
            let root = std::fs::canonicalize(fixture.checkout.root()).unwrap();
            let key = imp::key_for(&root);
            let (started, core) = Self::begin(&fixture.directory, &key, &root);
            Self {
                fixture,
                _started: Some(started),
                core,
                key,
                root,
            }
        }

        /// Start a watcher epoch, watch the worktree and publish its first counter.
        fn begin(directory: &Path, key: &str, root: &Path) -> (imp::Started, imp::Core) {
            let started = imp::start(directory, &AtomicBool::new(false))
                .unwrap()
                .expect("no other watcher holds the lock");
            let mut core = imp::Core::new(started.epoch.clone(), Arc::new(imp::Stats::default()));
            core.watch(key.to_string(), root.to_path_buf());
            core.flush(directory);
            (started, core)
        }

        fn restart(&mut self) {
            self._started = None;
            let (started, core) = Self::begin(&self.fixture.directory, &self.key, &self.root);
            self._started = Some(started);
            self.core = core;
        }

        /// A file changes and the file system reports it.
        fn change(&mut self, name: &str) {
            let path = self.root.join(name);
            std::fs::write(&path, "x\n").unwrap();
            let event =
                notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
                    .add_path(path);
            self.core.record(Ok(event));
        }

        fn flush(&mut self) {
            self.core.flush(&self.fixture.directory);
        }

        fn seen_path(&self) -> PathBuf {
            self.fixture.directory.join(SEEN).join(&self.key)
        }

        fn seen(&self) -> String {
            std::fs::read_to_string(self.seen_path()).unwrap()
        }

        /// The identity of `seen/<key>`: every write renames a new file over it.
        fn seen_identity(&self) -> (u64, std::time::SystemTime) {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(self.seen_path()).unwrap();
            (metadata.ino(), metadata.modified().unwrap())
        }

        fn observe(&self) -> imp::Observed {
            imp::observe(&self.fixture.directory, &self.root, &self.fixture.base)
                .expect("a live watcher with a published counter")
        }

        fn claim(&self, observed: &imp::Observed) {
            assert!(imp::claim(&self.fixture.directory, observed));
        }

        fn git(&self) -> Vec<String> {
            self.fixture
                .checkout
                .changed_files(&self.fixture.base)
                .unwrap()
        }

        fn answer(&self, observed: &imp::Observed, files: &[String]) {
            imp::write_answer(&self.fixture.directory, observed, files);
        }
    }

    #[test]
    fn a_change_after_a_claim_is_never_answered_from_the_old_answer() {
        // Race (a), the dangerous order, and race (c): two commands observe
        // the same counter and ask Git before a change; both store their
        // answers only after the watcher acted on the change.
        let mut driven = Driven::new();
        let first = driven.observe();
        driven.claim(&first);
        let first_files = driven.git();
        let claim_path = imp::claim_path(&driven.fixture.directory, &first);
        let claim_identity = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&claim_path).unwrap().ino()
        };
        let second = driven.observe();
        driven.claim(&second);
        {
            use std::os::unix::fs::MetadataExt;
            let shared = std::fs::metadata(&claim_path).unwrap().ino();
            assert_eq!(
                shared, claim_identity,
                "a second claim of one counter rewrote it"
            );
        }
        let second_files = driven.git();
        let before = driven.seen();
        driven.change("new.txt");
        driven.flush();
        assert_ne!(
            driven.seen(),
            before,
            "a claimed counter must move on a change"
        );
        driven.answer(&first, &first_files);
        driven.answer(&second, &second_files);
        let (files, source) = driven.fixture.ask();
        assert_eq!(source, Source::Git);
        assert_eq!(files, ["new.txt"]);
    }

    #[test]
    fn an_answer_stored_before_the_flush_is_outdated_by_it() {
        // Race (b): the answer lands after the event arrived but before the
        // watcher acted on it.
        let mut driven = Driven::new();
        let observed = driven.observe();
        driven.claim(&observed);
        let files = driven.git();
        driven.change("new.txt");
        driven.answer(&observed, &files);
        driven.flush();
        let (files, source) = driven.fixture.ask();
        assert_eq!(source, Source::Git);
        assert_eq!(files, ["new.txt"]);
    }

    #[test]
    fn a_claim_landing_before_the_flush_moves_the_counter() {
        // Race (b), claim side: the claim lands between the event and the
        // watcher's check, so the check must see it.
        let mut driven = Driven::new();
        let observed = driven.observe();
        driven.change("new.txt");
        driven.claim(&observed);
        let before = driven.seen();
        driven.flush();
        assert_ne!(driven.seen(), before);
        driven.answer(&observed, &driven.git());
        assert_eq!(driven.fixture.ask().1, Source::Git);
    }

    #[test]
    fn a_claim_after_a_skipped_change_stores_an_answer_that_includes_it() {
        // Race (a), the order the skip relies on: the command observed the
        // counter before the change but claims it only after the watcher
        // decided to skip, so Git runs after the change and sees it.
        let mut driven = Driven::new();
        let observed = driven.observe();
        let identity = driven.seen_identity();
        driven.change("new.txt");
        driven.flush();
        assert_eq!(
            driven.seen_identity(),
            identity,
            "an unclaimed counter was rewritten"
        );
        driven.claim(&observed);
        let files = driven.git();
        assert_eq!(files, ["new.txt"]);
        driven.answer(&observed, &files);
        assert_eq!(
            driven.fixture.ask(),
            (vec!["new.txt".to_string()], Source::Reused)
        );
    }

    #[test]
    fn a_late_claim_of_an_older_counter_does_not_hide_a_newer_claim() {
        let mut driven = Driven::new();
        // A slow command observes the first counter...
        let slow = driven.observe();
        let early = driven.observe();
        driven.claim(&early);
        driven.change("first.txt");
        driven.flush();
        // ...a fast command claims the second counter and asks Git...
        let fast = driven.observe();
        assert_ne!(fast.token(), slow.token());
        driven.claim(&fast);
        let fast_files = driven.git();
        // ...and only then does the slow command claim its older counter.
        driven.claim(&slow);
        driven.change("second.txt");
        driven.flush();
        driven.answer(&fast, &fast_files);
        let (files, source) = driven.fixture.ask();
        assert_eq!(source, Source::Git);
        assert_eq!(files, ["first.txt", "second.txt"]);
        // The sweep removes the orphaned claim and keeps the current one.
        let current = driven.observe();
        driven.claim(&current);
        driven.core.sweep_claims(&driven.fixture.directory);
        assert!(!imp::claim_path(&driven.fixture.directory, &slow).exists());
        assert!(imp::claim_path(&driven.fixture.directory, &current).exists());
    }

    #[test]
    fn a_restarted_watcher_ignores_the_previous_epochs_claims_and_answers() {
        // Race (d).
        let mut driven = Driven::new();
        let observed = driven.observe();
        driven.claim(&observed);
        driven.answer(&observed, &driven.git());
        assert_eq!(driven.fixture.ask().1, Source::Reused);
        driven.restart();
        driven.change("new.txt");
        driven.flush();
        let (files, source) = driven.fixture.ask();
        assert_eq!(source, Source::Git);
        assert_eq!(files, ["new.txt"]);
    }

    #[test]
    fn an_unchanged_or_unclaimed_counter_is_never_rewritten() {
        let mut driven = Driven::new();
        let identity = driven.seen_identity();
        for _ in 0..3 {
            driven.flush();
        }
        assert_eq!(
            driven.seen_identity(),
            identity,
            "an unchanged counter was rewritten"
        );
        for name in ["one.txt", "two.txt", "three.txt"] {
            driven.change(name);
            driven.flush();
        }
        assert_eq!(
            driven.seen_identity(),
            identity,
            "an unclaimed counter was rewritten"
        );
        let stats = &driven.core.stats;
        assert_eq!(load(&stats.published_initial), 1);
        assert_eq!(load(&stats.published_claimed), 0);
        assert_eq!(load(&stats.skipped_unclaimed), 3);
    }

    #[test]
    fn stale_temporaries_are_removed_at_startup() {
        let fixture = Fixture::new();
        let directory = &fixture.directory;
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        let mut stale = Vec::new();
        for folder in [WANTED, ANSWERS, SEEN] {
            std::fs::create_dir_all(directory.join(folder)).unwrap();
            stale.push(directory.join(folder).join(".0123456789ab.4242.7"));
        }
        stale.push(directory.join(".epoch.4242.3"));
        for path in &stale {
            std::fs::write(path, "x").unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        // A command may be mid-write right now: a fresh temporary stays.
        let fresh = directory.join(ANSWERS).join(".0123456789ab.4243.0");
        std::fs::write(&fresh, "x").unwrap();
        let started = imp::start(directory, &AtomicBool::new(false))
            .unwrap()
            .expect("no other watcher holds the lock");
        for path in &stale {
            assert!(!path.exists(), "{} was left behind", path.display());
        }
        assert!(fresh.exists(), "a temporary being written was removed");
        drop(started);
    }
}
