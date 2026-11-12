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
//! meaning. The watcher publishes one counter per worktree that moves whenever
//! the file system reports any change under it. A command that scans a
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
//!   loss moves every counter.
//! - **Bounded staleness, read-only commands only.** A counter is published up
//!   to [`FLUSH_EVERY`] after the change, so a reused answer can be that much
//!   older than a fresh one. Only commands that call
//!   [`allow_reuse_for_this_command`] reuse answers; anything that decides
//!   whether work may land keeps asking Git.

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

/// How often the watcher publishes counters that moved.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) const FLUSH_EVERY: Duration = Duration::from_millis(100);
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
    let files = {
        let _span = crate::phase_telemetry::span("changed_files_asked_git");
        checkout.changed_files(base)?
    };
    if let Some(observed) = before {
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::{Duration, Instant, SystemTime};

    use notify::{RecursiveMode, Watcher};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};

    use super::{
        ANSWER_MAX_AGE, ANSWERS, EPOCH, FLUSH_EVERY, LOCK, READY, SEEN, WANTED, WANTED_ACTIVE,
        WANTED_KEEP, WANTED_REFRESH,
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
    fn key_for(canonical: &Path) -> String {
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

    /// Store Git's answer beside what was observed *before* Git ran. A change
    /// during the scan moves the counter past the stored one, so the answer is
    /// then never reused.
    pub(super) fn write_answer(directory: &Path, observed: &Observed, files: &[String]) {
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

    struct Watched {
        path: PathBuf,
        counter: u64,
        unpublished: bool,
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

    fn publish(directory: &Path, epoch: &str, key: &str, watched: &mut Watched) {
        let line = format!("{epoch}:{}", watched.counter);
        if write_atomically(&directory.join(SEEN).join(key), line.as_bytes()).is_ok() {
            watched.unpublished = false;
        }
    }

    /// Watch until `stop` is set. Returns at once when another watcher holds
    /// the lock.
    pub(crate) fn run(directory: &Path, stop: &AtomicBool) -> io::Result<()> {
        for name in [WANTED, SEEN, ANSWERS] {
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
        let Some(_lock) = lock else {
            return Ok(());
        };
        // A new epoch outdates every counter and answer of earlier watchers.
        for entry in fs::read_dir(directory.join(SEEN))?.flatten() {
            hint(fs::remove_file(entry.path()));
        }
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
        let Some(_ready) = ready else {
            return Ok(());
        };

        let (sender, events) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            hint(sender.send(event));
        })
        .map_err(io::Error::other)?;
        let mut watching: HashMap<String, Watched> = HashMap::new();
        let mut next_review = Instant::now();

        while !stop.load(Ordering::Relaxed) {
            if Instant::now() >= next_review {
                next_review = Instant::now() + Duration::from_secs(1);
                review(directory, &mut watcher, &mut watching);
            }
            let mut received = match events.recv_timeout(FLUSH_EVERY) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("file watcher closed its channel"));
                }
            };
            let mut drained = 0;
            while let Some(event) = received.take() {
                record(&mut watching, event);
                drained += 1;
                if drained < 4096 {
                    received = events.try_recv().ok();
                }
            }
            for (key, watched) in &mut watching {
                if watched.unpublished {
                    publish(directory, &epoch, key, watched);
                }
            }
        }
        Ok(())
    }

    /// Start watching newly requested worktrees and stop watching forgotten ones.
    fn review(
        directory: &Path,
        watcher: &mut notify::RecommendedWatcher,
        watching: &mut HashMap<String, Watched>,
    ) {
        let wanted = wanted(directory);
        let mut changed_watch_set = false;
        for (key, path, age) in &wanted {
            let keep = *age < WANTED_KEEP && path.is_dir();
            if keep && !watching.contains_key(key) {
                if watcher.watch(path, RecursiveMode::Recursive).is_ok() {
                    watching.insert(
                        key.clone(),
                        Watched {
                            path: path.clone(),
                            counter: 0,
                            unpublished: true,
                        },
                    );
                    changed_watch_set = true;
                }
            } else if !keep {
                if let Some(watched) = watching.remove(key) {
                    hint(watcher.unwatch(&watched.path));
                    changed_watch_set = true;
                }
                hint(fs::remove_file(directory.join(SEEN).join(key)));
                hint(fs::remove_file(directory.join(ANSWERS).join(key)));
                hint(fs::remove_file(directory.join(WANTED).join(key)));
            }
        }
        // Changing the watch set can restart the underlying event stream, and
        // events in that gap are lost: treat every worktree as changed.
        if changed_watch_set {
            change_all(watching);
        }
    }

    fn change_all(watching: &mut HashMap<String, Watched>) {
        for watched in watching.values_mut() {
            watched.counter += 1;
            watched.unpublished = true;
        }
    }

    fn record(watching: &mut HashMap<String, Watched>, event: notify::Result<notify::Event>) {
        let event = match event {
            Ok(event) if !event.need_rescan() => event,
            // An error or a flagged loss: nothing is known about any worktree.
            _ => {
                change_all(watching);
                return;
            }
        };
        // Reads are reported on Linux (inotify opens and closes), and a lease
        // scan itself reads the worktree: only changes move a counter.
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }
        for path in &event.paths {
            for watched in watching.values_mut() {
                if path.starts_with(&watched.path) {
                    watched.counter += 1;
                    watched.unpublished = true;
                }
            }
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
                _holder: holder,
            }
        }

        fn start_watcher(&mut self) {
            let directory = self.directory.clone();
            let stop = Arc::clone(&self.stop);
            self.stop.store(false, Ordering::Relaxed);
            self.watcher = Some(std::thread::spawn(move || {
                imp::run(&directory, &stop).unwrap();
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
}
