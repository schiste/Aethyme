//! The host-wide artifact max age (`[budget] artifact_max_age_hours`,
//! default 24, `0` off) for what lives outside the ledger's own classes.
//!
//! The ledger's classes apply it themselves: gate cache entries unused for
//! the max age go whatever the budget ([`super::evict::gate_entry_expired`]),
//! and superseded local build units are pruned out of every gate cache each
//! pass ([`super::gate_prune`]). The idle strip removes the Cargo targets of
//! sessions idle for the max age (`broker::strip_idle`). This step covers
//! the rest, each pass, within one deadline:
//!
//! - **Cargo targets in session scratch directories**
//!   (`<worktree root>/.aethyme-session-scratch/s<id>-*/` and one level
//!   below), only in a container carrying the broker's scratch marker;
//! - **Cargo targets in the main checkouts of registered repositories**
//!   (directories named `target` up to three levels deep, never inside a
//!   hidden directory or `node_modules`), only when Git says the directory
//!   is untracked (no tracked file in it) and ignored;
//! - **empty leftover worktree roots** in Aethyme's `worktrees` containers.
//!
//! A Cargo target is only ever a directory carrying Cargo's `CACHEDIR.TAG`
//! (the standard signature). It is in use while a build holds one of its
//! profiles' `.cargo-lock` files (Cargo's own build lock, `flock`), and it
//! counts as used at the newest of: the modification times of the target,
//! its profiles and their `.fingerprint`, `deps`, `build` and `incremental`
//! directories (any compile renames new files into them), and the access and
//! modification times of `.rustc_info.json` (Cargo reads it on every
//! invocation) and of each `.cargo-lock`. Directory access times are never
//! read: this step lists those directories itself. One unused for the max
//! age is renamed aside while this worker holds every profile's lock (a
//! build starting then waits), and deleted after; one in use within the max
//! age keeps its newest generations, and the superseded generations of its
//! local packages unused for the max age are pruned, again only under every
//! profile's lock ([`crate::cargo_target_prune::UnitScope::Worker`]).
//! Removal costs a rebuild, never a wrong result. The profiles are listed
//! and the last use read again once the locks are held, so a build that ran
//! just before is seen; a profile directory counts as soon as it holds
//! `.cargo-lock`, which Cargo creates first. Known limit: a Cargo process
//! already blocked on a lock the worker holds opened the lock file by path
//! before the rename; once the worker releases it, that process holds a lock
//! on the renamed (doomed) tree, not on the new target it then builds into,
//! so a second build starting at the same moment is not excluded from it.
//! Each builds correct output; at worst one rebuilds.
//!
//! A leftover worktree root is a directory in `<host state, host cache or
//! storage root>/worktrees` that no registered repository still uses (no
//! registered worktree root whose repository exists there, and its
//! `.aethyme-worktree-root.json`, if any, names a repository that is gone, or
//! it is a quick-test pool), that holds Aethyme's own names (evidence it is
//! one), nothing but Aethyme's markers and empty directories (no session, no
//! recovery archive, no file of anyone's), and nothing in it changed for
//! the max age. It is scanned again right before it is deleted.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use super::check::WorkerLock;
use super::evict::duration_ms;
use super::{Budgets, Root, RootKind, evict};

/// The longest one pass spends on this step, deletion included.
pub const ARTIFACT_AGE_BUDGET: Duration = Duration::from_secs(60);
/// The class names this step reports under.
pub const SESSION_SCRATCH_BUILD: &str = "session_scratch_build";
pub const MAIN_CHECKOUT_TARGET: &str = "main_checkout_target";
pub const LEFTOVER_WORKTREE_ROOT: &str = "leftover_worktree_root";
/// Entries read from one directory listing, at most.
const LISTING_MAX: usize = 4_096;
/// How deep below a main checkout targets are looked for.
const MAIN_CHECKOUT_DEPTH: usize = 3;
/// Directories listed per main checkout, at most.
const MAIN_CHECKOUT_DIRECTORIES_MAX: usize = 512;
/// Scratch directories looked into per worktree root, at most.
const SCRATCH_ENTRIES_MAX: usize = 1_024;
/// Entries a leftover worktree root may hold, at most, to be judged.
const LEFTOVER_ENTRIES_MAX: usize = 256;
/// How deep a leftover worktree root is scanned.
const LEFTOVER_DEPTH_MAX: usize = 8;
/// The beginning of the cargo configuration the broker writes beside
/// session worktrees (`WORKTREE_CARGO_CONFIG`).
const BROKER_CARGO_CONFIG_PREFIX: &str = "# Written once by the Aethyme broker";

/// What the step did to one class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassOutcome {
    /// Artifacts found (Cargo targets, or candidate roots).
    pub found: u64,
    /// Removed whole because unused for the max age, and their bytes.
    pub removed_for_age: u64,
    pub removed_for_age_bytes: u64,
    /// Superseded local build units pruned out of targets in use (no
    /// pile-up), and their logical bytes.
    pub pruned_for_pile_up: u64,
    pub pruned_for_pile_up_bytes: u64,
    /// Kept because a build holds them or they were used within the max
    /// age (targets), or recently changed (roots).
    pub kept_in_use: u64,
    /// Kept because nothing proves them removable (tracked or not ignored,
    /// no tag, a foreign file, a repository that exists).
    pub kept_unproven: u64,
    /// The age of the least recently used artifact still present after the
    /// pass, among those with a known age: within the max age unless one is
    /// kept as unproven or in use by a running build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_kept_age_ms: Option<u64>,
}

impl ClassOutcome {
    fn kept_aged(&mut self, age_ms: u64) {
        self.oldest_kept_age_ms = Some(self.oldest_kept_age_ms.unwrap_or(0).max(age_ms));
    }
}

/// What the step did this pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactAgeReport {
    pub max_age_hours: u64,
    /// By class name ([`SESSION_SCRATCH_BUILD`], [`MAIN_CHECKOUT_TARGET`],
    /// [`LEFTOVER_WORKTREE_ROOT`]).
    pub classes: BTreeMap<String, ClassOutcome>,
    /// `done`, `deadline` or `lock_lost`.
    pub stop: String,
}

/// What is known about one Cargo target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildDirFacts {
    /// It carries Cargo's `CACHEDIR.TAG`.
    pub tagged: bool,
    /// Proven to hold no tracked file and to be ignored (or outside every
    /// checkout); `None` when that could not be proven either way.
    pub untracked: Option<bool>,
    /// A build holds one of its profiles' `.cargo-lock`.
    pub build_running: bool,
    /// When it was last used (epoch ms); 0 when unknown.
    pub last_use_ms: u64,
}

/// What becomes of one Cargo target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildDirVerdict {
    /// Unused for the max age: removed whole.
    Remove,
    /// Used within the max age: kept, its superseded generations pruned.
    Active,
    /// A build holds it now.
    BuildRunning,
    /// Nothing proves it removable; the reason.
    Unproven(&'static str),
}

/// The verdict on a Cargo target at `now_ms` under `max_age`.
pub fn judge_build_dir(facts: BuildDirFacts, now_ms: u64, max_age: Duration) -> BuildDirVerdict {
    if !facts.tagged {
        return BuildDirVerdict::Unproven("no_cachedir_tag");
    }
    if facts.untracked != Some(true) {
        return BuildDirVerdict::Unproven("tracked_or_not_ignored");
    }
    if facts.build_running {
        return BuildDirVerdict::BuildRunning;
    }
    if facts.last_use_ms == 0 {
        return BuildDirVerdict::Unproven("last_use_unknown");
    }
    if now_ms.saturating_sub(facts.last_use_ms) < duration_ms(max_age) {
        BuildDirVerdict::Active
    } else {
        BuildDirVerdict::Remove
    }
}

/// What a scan of a candidate leftover worktree root found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkerScan {
    /// Nothing but Aethyme's marker files and directories.
    pub only_markers: bool,
    /// An entry with one of Aethyme's names (`.aethyme*`).
    pub evidence: bool,
    /// The newest modification time inside, the root included (epoch ms).
    pub newest_ms: u64,
}

/// What is known about one candidate leftover worktree root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeftoverFacts {
    /// A registered worktree root whose repository still exists.
    pub registered_live: bool,
    /// A quick-test pool (`aethyme-broker-quick-test-*`).
    pub quick_test: bool,
    /// Whether the repository its worktree-root marker names exists; `None`
    /// without a marker.
    pub marker_repository_exists: Option<bool>,
    /// `None` when it could not be scanned completely.
    pub scan: Option<MarkerScan>,
}

/// Whether a candidate leftover worktree root may be removed at `now_ms`;
/// `Err` is why it stays.
pub fn judge_leftover_root(
    facts: LeftoverFacts,
    now_ms: u64,
    max_age: Duration,
) -> Result<(), &'static str> {
    if facts.registered_live {
        return Err("registered");
    }
    if !facts.quick_test && facts.marker_repository_exists == Some(true) {
        return Err("repository_exists");
    }
    let Some(scan) = facts.scan else {
        return Err("not_scanned");
    };
    if !scan.only_markers {
        return Err("holds_content");
    }
    if !scan.evidence && !facts.quick_test {
        return Err("not_aethyme");
    }
    if scan.newest_ms == 0 || now_ms.saturating_sub(scan.newest_ms) < duration_ms(max_age) {
        return Err("recent");
    }
    Ok(())
}

/// Whether `name` (in the directory named `parent`) is one of Aethyme's own
/// marker files, or Finder metadata, that a leftover root may hold.
fn is_marker_file(parent: Option<&str>, name: &str, path: &Path) -> bool {
    match name {
        ".DS_Store"
        | crate::broker::WORKTREE_ROOT_MARKER
        | crate::session_scratch::SCRATCH_MARKER => true,
        _ if name.starts_with(".aethyme-reclaim-plan-") && name.ends_with(".json") => true,
        _ if name.starts_with(".aethyme-finished.") => true,
        "config.toml" if parent == Some(".cargo") => {
            fs::metadata(path).is_ok_and(|metadata| metadata.len() <= 16 * 1024)
                && fs::read_to_string(path)
                    .is_ok_and(|text| text.starts_with(BROKER_CARGO_CONFIG_PREFIX))
        }
        _ => false,
    }
}

/// Scan `dir` (bounded): only markers, evidence, newest change. `None` when
/// it cannot be read completely (an unreadable entry, a non-UTF-8 name, too
/// many entries or levels).
pub fn scan_markers(dir: &Path) -> Option<MarkerScan> {
    let top = fs::symlink_metadata(dir).ok()?;
    if !top.is_dir() {
        return None;
    }
    let mut scan = MarkerScan {
        only_markers: true,
        evidence: false,
        newest_ms: super::mtime_ms(&top),
    };
    let mut pending = vec![(dir.to_path_buf(), 0_usize)];
    let mut seen = 0_usize;
    while let Some((next, depth)) = pending.pop() {
        let parent = next
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string);
        for entry in fs::read_dir(&next).ok()? {
            let entry = entry.ok()?;
            seen += 1;
            if seen > LEFTOVER_ENTRIES_MAX {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).ok()?;
            scan.newest_ms = scan.newest_ms.max(super::mtime_ms(&metadata));
            if name.starts_with(".aethyme") {
                scan.evidence = true;
            }
            if metadata.file_type().is_symlink() {
                scan.only_markers = false;
            } else if metadata.is_dir() {
                if depth + 1 > LEFTOVER_DEPTH_MAX {
                    return None;
                }
                pending.push((path, depth + 1));
            } else if !is_marker_file(parent.as_deref(), &name, &path) {
                scan.only_markers = false;
            }
        }
    }
    Some(scan)
}

fn is_real_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// UTF-8 names in `dir`, at most [`LISTING_MAX`], sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .take(LISTING_MAX)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

fn is_staged_name(name: &str) -> bool {
    name.starts_with(super::check::STAGED_DELETION_PREFIX)
}

/// Cargo targets found, and staged leftovers of earlier removals beside
/// them.
#[derive(Debug, Default)]
struct Found {
    targets: Vec<PathBuf>,
    staged: Vec<PathBuf>,
}

/// The Cargo targets of the main checkout `checkout`: directories named
/// `target` carrying the tag, at most [`MAIN_CHECKOUT_DEPTH`] levels down,
/// never inside a hidden directory, `node_modules` or a target.
fn main_checkout_targets(checkout: &Path, found: &mut Found) {
    let mut pending = vec![(checkout.to_path_buf(), 0_usize)];
    let mut listed = 0_usize;
    while let Some((dir, depth)) = pending.pop() {
        listed += 1;
        if listed > MAIN_CHECKOUT_DIRECTORIES_MAX {
            break;
        }
        for name in names(&dir) {
            let path = dir.join(&name);
            if !is_real_directory(&path) {
                continue;
            }
            if is_staged_name(&name) {
                found.staged.push(path);
                continue;
            }
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            if name == "target" && crate::auto_cleanup::has_cache_directory_tag(&path) {
                found.targets.push(path);
                continue;
            }
            if depth + 1 < MAIN_CHECKOUT_DEPTH {
                pending.push((path, depth + 1));
            } else if depth + 1 == MAIN_CHECKOUT_DEPTH {
                // The deepest level is probed, not listed.
                let target = path.join("target");
                if is_real_directory(&target)
                    && crate::auto_cleanup::has_cache_directory_tag(&target)
                {
                    found.targets.push(target);
                }
            }
        }
    }
}

/// The Cargo targets in the session scratch directories of the worktree
/// root `root`: a scratch directory's children and grandchildren carrying
/// the tag, in a container carrying the broker's marker.
fn scratch_targets(root: &Path, found: &mut Found) {
    let container = root.join(crate::session_scratch::SCRATCH_CONTAINER);
    if !is_real_directory(&container)
        || !fs::symlink_metadata(container.join(crate::session_scratch::SCRATCH_MARKER))
            .is_ok_and(|metadata| metadata.is_file())
    {
        return;
    }
    for entry in names(&container)
        .into_iter()
        .filter(|name| {
            name.strip_prefix('s')
                .and_then(|rest| rest.chars().next())
                .is_some_and(|first| first.is_ascii_digit())
        })
        .take(SCRATCH_ENTRIES_MAX)
    {
        let entry = container.join(entry);
        if !is_real_directory(&entry) {
            continue;
        }
        for child in names(&entry) {
            let path = entry.join(&child);
            if !is_real_directory(&path) {
                continue;
            }
            if is_staged_name(&child) {
                found.staged.push(path);
            } else if crate::auto_cleanup::has_cache_directory_tag(&path) {
                found.targets.push(path);
            } else if !child.starts_with('.') && child != "node_modules" {
                for grandchild in names(&path) {
                    let inner = path.join(&grandchild);
                    if !is_real_directory(&inner) {
                        continue;
                    }
                    if is_staged_name(&grandchild) {
                        found.staged.push(inner);
                    } else if crate::auto_cleanup::has_cache_directory_tag(&inner) {
                        found.targets.push(inner);
                    }
                }
            }
        }
    }
}

/// The profile directories of a Cargo target: those holding `.fingerprint`,
/// directly (`debug`) or under a target triple (`<triple>/debug`).
fn profiles(target: &Path) -> Vec<PathBuf> {
    // Cargo creates a profile's `.cargo-lock` before anything else in it, so
    // a directory holding only the lock is a profile being built.
    let is_profile = |path: &Path| {
        is_real_directory(&path.join(".fingerprint"))
            || fs::symlink_metadata(path.join(".cargo-lock"))
                .is_ok_and(|metadata| metadata.is_file())
    };
    let mut found = Vec::new();
    for name in names(target) {
        let path = target.join(&name);
        if !is_real_directory(&path) || is_staged_name(&name) {
            continue;
        }
        if is_profile(&path) {
            found.push(path);
            continue;
        }
        for inner in names(&path) {
            let inner = path.join(inner);
            if is_real_directory(&inner) && is_profile(&inner) {
                found.push(inner);
            }
        }
    }
    found
}

#[cfg(test)]
thread_local! {
    /// Test hook: runs on a target's path after its profiles were first
    /// listed, just before their build locks are taken.
    pub(super) static BEFORE_LOCK_FOR_TEST: std::cell::RefCell<Option<Box<dyn Fn(&Path)>>> =
        const { std::cell::RefCell::new(None) };
}

/// Take the build locks of `target`'s profiles, then list its profiles and
/// read its last use again under them: a build that ran between the first
/// listing and the locks is seen, and a profile that appeared meanwhile
/// (not locked) counts as a running build. `None` for the locks when a build
/// holds one.
fn lock_then_read(target: &Path) -> (Option<ProfileLocks>, u64) {
    let first = profiles(target);
    #[cfg(test)]
    BEFORE_LOCK_FOR_TEST.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook(target);
        }
    });
    let locks = lock_profiles(&first);
    let current = profiles(target);
    let last_use = last_use_ms(target, &current);
    let unlocked_new = current.iter().any(|profile| !first.contains(profile));
    if unlocked_new {
        return (None, last_use);
    }
    (locks, last_use)
}

/// Cargo's build locks on a target's profiles, held by this worker.
struct ProfileLocks(#[allow(dead_code)] Vec<fs::File>);

/// Take every profile's `.cargo-lock` (Cargo's build lock) without
/// waiting; `None` when a build holds one or one cannot be opened. A profile
/// without the file has never been built by Cargo and needs none.
fn lock_profiles(profiles: &[PathBuf]) -> Option<ProfileLocks> {
    let mut held = Vec::new();
    for profile in profiles {
        let file = match fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(profile.join(".cargo-lock"))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        if file.try_lock().is_err() {
            return None;
        }
        held.push(file);
    }
    Some(ProfileLocks(held))
}

fn access_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .accessed()
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// When a target was last used (see the module): directory modification
/// times, and the access and modification times of the files Cargo reads
/// on every invocation but this worker never reads.
fn last_use_ms(target: &Path, profiles: &[PathBuf]) -> u64 {
    let mut newest = 0_u64;
    let mut modified = |path: &Path| {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            newest = newest.max(super::mtime_ms(&metadata));
        }
    };
    modified(target);
    for profile in profiles {
        modified(profile);
        for name in [".fingerprint", "deps", "build", "incremental"] {
            modified(&profile.join(name));
        }
    }
    let mut files = vec![target.join(".rustc_info.json")];
    files.extend(profiles.iter().map(|profile| profile.join(".cargo-lock")));
    for file in files {
        if let Ok(metadata) = fs::symlink_metadata(&file)
            && metadata.is_file()
        {
            newest = newest
                .max(super::mtime_ms(&metadata))
                .max(access_ms(&metadata));
        }
    }
    newest
}

/// Variables that route Git to another repository, index or configuration.
/// The detached worker inherits the environment of the command that spawned
/// it, which may run inside a Git hook of another repository.
const GIT_ROUTING_VARIABLES: [&str; 11] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

/// Run one bounded Git query in `dir`; its exit code, `None` when it did not
/// complete. `inherited` is added to the environment first (tests model a
/// hook's environment with it); every routing variable is then dropped,
/// pathspecs are literal and no file-system monitor is asked.
fn git_exit_code_inheriting(dir: &Path, args: &[&str], inherited: &[(&str, &str)]) -> Option<i32> {
    let mut command = crate::git::git_command();
    command.envs(inherited.iter().copied());
    for variable in GIT_ROUTING_VARIABLES {
        command.env_remove(variable);
    }
    command
        .args(["--literal-pathspecs", "-c", "core.fsmonitor=false", "-C"])
        .arg(dir)
        .args(args)
        .current_dir(dir);
    match crate::bounded_output::output_limited(&mut command, crate::git::git_timeout(), 4096) {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) => output.status.code(),
        _ => None,
    }
}

/// Whether `relative` in the checkout at `checkout` holds no tracked file
/// and is ignored: `Some(true)`; `Some(false)` when it is tracked or not
/// ignored; `None` when Git could not tell.
fn untracked_and_ignored(checkout: &Path, relative: &str) -> Option<bool> {
    untracked_and_ignored_inheriting(checkout, relative, &[])
}

fn untracked_and_ignored_inheriting(
    checkout: &Path,
    relative: &str,
    inherited: &[(&str, &str)],
) -> Option<bool> {
    let git = |args: &[&str]| git_exit_code_inheriting(checkout, args, inherited);
    match git(&["ls-files", "--error-unmatch", "--", relative])? {
        0 => return Some(false),
        1 => {}
        _ => return None,
    }
    match git(&["check-ignore", "-q", "--", relative])? {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// The per-pass state of the step.
struct Step<'a> {
    max_age: Duration,
    now_ms: u64,
    deadline: Instant,
    pace: Duration,
    lock: Option<&'a mut WorkerLock>,
    report: ArtifactAgeReport,
}

impl Step<'_> {
    fn stopped(&mut self) -> bool {
        if self.report.stop != "done" {
            return true;
        }
        if self
            .lock
            .as_deref()
            .is_some_and(|lock| lock.lost() || !lock.held())
        {
            self.report.stop = "lock_lost".into();
            return true;
        }
        if Instant::now() >= self.deadline {
            self.report.stop = "deadline".into();
            return true;
        }
        false
    }

    fn outcome(&mut self, class: &str) -> &mut ClassOutcome {
        self.report.classes.entry(class.to_string()).or_default()
    }

    fn delete(&mut self, path: &Path) -> (bool, u64) {
        let deadline = self.deadline;
        let lock = &mut self.lock;
        let mut beat = || {
            if let Some(lock) = lock.as_deref_mut() {
                lock.beat();
            }
        };
        evict::delete_tree_measured(path, &mut beat, Some(deadline)).unwrap_or((false, 0))
    }

    fn pause(&self) {
        if !self.pace.is_zero() {
            std::thread::sleep(self.pace);
        }
    }

    /// Remove staged leftovers of earlier interrupted removals.
    fn sweep(&mut self, staged: &[PathBuf]) {
        for path in staged {
            if self.stopped() {
                return;
            }
            self.delete(path);
        }
    }

    /// Judge and act on one Cargo target of `class`. `untracked` is the
    /// tracked-state proof (`Some(true)` for scratch, outside any checkout).
    fn handle_target(&mut self, class: &'static str, target: &Path, untracked: Option<bool>) {
        let _span = crate::phase_telemetry::span("storage_artifact_age_target");
        self.outcome(class).found += 1;
        let (locks, last_use) = lock_then_read(target);
        let facts = BuildDirFacts {
            tagged: crate::auto_cleanup::has_cache_directory_tag(target),
            untracked,
            build_running: locks.is_none(),
            last_use_ms: last_use,
        };
        let age = self.now_ms.saturating_sub(last_use);
        match judge_build_dir(facts, self.now_ms, self.max_age) {
            BuildDirVerdict::Unproven(_) => {
                crate::phase_telemetry::add(kept_unproven_counter(class), Duration::ZERO);
                let outcome = self.outcome(class);
                outcome.kept_unproven += 1;
                if last_use > 0 {
                    outcome.kept_aged(age);
                }
            }
            BuildDirVerdict::BuildRunning => {
                crate::phase_telemetry::add(kept_in_use_counter(class), Duration::ZERO);
                let outcome = self.outcome(class);
                outcome.kept_in_use += 1;
                outcome.kept_aged(age);
            }
            BuildDirVerdict::Active => {
                let outcome = self.outcome(class);
                outcome.kept_in_use += 1;
                outcome.kept_aged(age);
                crate::phase_telemetry::add(kept_in_use_counter(class), Duration::ZERO);
                // Only with every profile's build lock held.
                if locks.is_some() {
                    self.prune(class, target);
                }
            }
            BuildDirVerdict::Remove => {
                let _span = crate::phase_telemetry::span("storage_artifact_age_remove");
                let Some(parent) = target.parent() else {
                    return;
                };
                let aside = parent.join(format!(
                    "{}{}-{}",
                    super::check::STAGED_DELETION_PREFIX,
                    std::process::id(),
                    super::now_ms()
                ));
                // Renamed while every profile's lock is held: a build that
                // starts now waits, then builds afresh.
                let renamed = fs::rename(target, &aside).is_ok();
                drop(locks);
                if !renamed {
                    self.outcome(class).kept_unproven += 1;
                    return;
                }
                let (_, bytes) = self.delete(&aside);
                crate::phase_telemetry::add(removed_counter(class), Duration::ZERO);
                let outcome = self.outcome(class);
                outcome.removed_for_age += 1;
                outcome.removed_for_age_bytes = outcome.removed_for_age_bytes.saturating_add(bytes);
                self.pause();
            }
        }
    }

    /// Prune superseded local generations unused for the max age out of
    /// the target (the caller holds every profile's build lock).
    fn prune(&mut self, class: &'static str, target: &Path) {
        let _span = crate::phase_telemetry::span("storage_artifact_age_prune");
        let Some(parent) = target.parent() else {
            return;
        };
        let staging = parent.join(format!(
            "{}{}-{}-pruned",
            super::check::STAGED_DELETION_PREFIX,
            std::process::id(),
            super::now_ms()
        ));
        let outcome = crate::cargo_target_prune::prune_superseded_units_in(
            target,
            &staging,
            u64::MAX,
            self.deadline,
            crate::cargo_target_prune::UnitScope::Worker(crate::cargo_target_prune::WorkerRule {
                now: SystemTime::now(),
                unused_for: self.max_age,
            }),
        );
        if is_real_directory(&staging) {
            self.delete(&staging);
        }
        for _ in 0..outcome.units {
            crate::phase_telemetry::add(pruned_counter(class), Duration::ZERO);
        }
        let report = self.outcome(class);
        report.pruned_for_pile_up += outcome.units;
        report.pruned_for_pile_up_bytes = report
            .pruned_for_pile_up_bytes
            .saturating_add(outcome.bytes);
        if outcome.units > 0 {
            self.pause();
        }
    }

    /// Judge and remove one candidate leftover worktree root.
    fn handle_leftover_root(&mut self, path: &Path, registered_live: bool) {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let facts = |scan: Option<MarkerScan>| LeftoverFacts {
            registered_live,
            quick_test: super::is_quick_test_pool(&name),
            marker_repository_exists: marker_repository_exists(path),
            scan,
        };
        let scan = scan_markers(path);
        let Some(first) = scan else {
            return;
        };
        if !first.evidence && !super::is_quick_test_pool(&name) {
            // Not one of Aethyme's: not even counted.
            return;
        }
        self.outcome(LEFTOVER_WORKTREE_ROOT).found += 1;
        let age = self.now_ms.saturating_sub(first.newest_ms);
        match judge_leftover_root(facts(scan), self.now_ms, self.max_age) {
            Ok(()) => {}
            Err("recent") => {
                let outcome = self.outcome(LEFTOVER_WORKTREE_ROOT);
                outcome.kept_in_use += 1;
                outcome.kept_aged(age);
                return;
            }
            Err(_) => {
                crate::phase_telemetry::add(
                    "storage_artifact_age_leftover_worktree_root_kept_unproven",
                    Duration::ZERO,
                );
                self.outcome(LEFTOVER_WORKTREE_ROOT).kept_unproven += 1;
                return;
            }
        }
        // Scanned again right before the deletion.
        if judge_leftover_root(facts(scan_markers(path)), super::now_ms(), self.max_age).is_err() {
            self.outcome(LEFTOVER_WORKTREE_ROOT).kept_unproven += 1;
            return;
        }
        let (done, bytes) = self.delete(path);
        if done {
            crate::phase_telemetry::add(
                "storage_artifact_age_leftover_worktree_root_removed",
                Duration::ZERO,
            );
            let outcome = self.outcome(LEFTOVER_WORKTREE_ROOT);
            outcome.removed_for_age += 1;
            outcome.removed_for_age_bytes = outcome.removed_for_age_bytes.saturating_add(bytes);
        }
        self.pause();
    }
}

/// Whether the repository the worktree-root marker in `root` names exists;
/// `None` without a marker. A marker that cannot be read or names nothing
/// counts as naming one that exists (the root is kept).
fn marker_repository_exists(root: &Path) -> Option<bool> {
    let marker = root.join(crate::broker::WORKTREE_ROOT_MARKER);
    match fs::symlink_metadata(&marker) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(true),
        Ok(_) => {}
    }
    let repository = fs::read(&marker)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("repository_root")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
        });
    Some(repository.is_none_or(|repository| repository.exists()))
}

fn removed_counter(class: &str) -> &'static str {
    match class {
        SESSION_SCRATCH_BUILD => "storage_artifact_age_session_scratch_build_removed",
        _ => "storage_artifact_age_main_checkout_target_removed",
    }
}

fn pruned_counter(class: &str) -> &'static str {
    match class {
        SESSION_SCRATCH_BUILD => "storage_artifact_age_session_scratch_build_pruned_unit",
        _ => "storage_artifact_age_main_checkout_target_pruned_unit",
    }
}

fn kept_in_use_counter(class: &str) -> &'static str {
    match class {
        SESSION_SCRATCH_BUILD => "storage_artifact_age_session_scratch_build_kept_in_use",
        _ => "storage_artifact_age_main_checkout_target_kept_in_use",
    }
}

fn kept_unproven_counter(class: &str) -> &'static str {
    match class {
        SESSION_SCRATCH_BUILD => "storage_artifact_age_session_scratch_build_kept_unproven",
        _ => "storage_artifact_age_main_checkout_target_kept_unproven",
    }
}

/// The main checkouts of registered repositories that exist, from the
/// worktree roots' registrations.
fn main_checkouts(roots: &[Root]) -> Vec<PathBuf> {
    let mut checkouts: BTreeSet<PathBuf> = BTreeSet::new();
    for root in roots
        .iter()
        .filter(|root| root.kind == RootKind::WorktreeRoot)
    {
        for repository in &root.repositories {
            if repository.join(".git").exists() && is_real_directory(repository) {
                checkouts.insert(repository.clone());
            }
        }
    }
    checkouts.into_iter().collect()
}

/// Run the step over `roots` (see the module); `None` when the max age is
/// off.
pub fn run(
    roots: &[Root],
    budgets: &Budgets,
    deadline: Instant,
    pace: Duration,
    lock: Option<&mut WorkerLock>,
) -> Option<ArtifactAgeReport> {
    let max_age = budgets.artifact_max_age()?;
    let _span = crate::phase_telemetry::span("storage_artifact_age");
    let mut step = Step {
        max_age,
        now_ms: super::now_ms(),
        deadline,
        pace,
        lock,
        report: ArtifactAgeReport {
            max_age_hours: budgets.artifact_max_age_hours,
            stop: "done".into(),
            ..ArtifactAgeReport::default()
        },
    };
    // Session scratch targets: outside every checkout.
    for root in roots
        .iter()
        .filter(|root| root.kind == RootKind::WorktreeRoot)
    {
        if step.stopped() {
            break;
        }
        let mut found = Found::default();
        {
            let _span = crate::phase_telemetry::span("storage_artifact_age_discover");
            scratch_targets(&root.path, &mut found);
        }
        step.sweep(&found.staged);
        for target in &found.targets {
            if step.stopped() {
                break;
            }
            step.handle_target(SESSION_SCRATCH_BUILD, target, Some(true));
        }
    }
    // Main-checkout targets: only untracked, ignored ones.
    for checkout in main_checkouts(roots) {
        if step.stopped() {
            break;
        }
        let mut found = Found::default();
        {
            let _span = crate::phase_telemetry::span("storage_artifact_age_discover");
            main_checkout_targets(&checkout, &mut found);
        }
        step.sweep(&found.staged);
        for target in &found.targets {
            if step.stopped() {
                break;
            }
            let untracked = target
                .strip_prefix(&checkout)
                .ok()
                .and_then(|relative| relative.to_str())
                .and_then(|relative| untracked_and_ignored(&checkout, relative));
            step.handle_target(MAIN_CHECKOUT_TARGET, target, untracked);
        }
    }
    // Leftover worktree roots in Aethyme's `worktrees` containers.
    {
        let _span = crate::phase_telemetry::span("storage_artifact_age_leftover_roots");
        let live: BTreeSet<&Path> = roots
            .iter()
            .filter(|root| root.kind == RootKind::WorktreeRoot)
            .filter(|root| {
                root.repositories
                    .iter()
                    .any(|repository| repository.join(".git").exists())
            })
            .map(|root| root.path.as_path())
            .collect();
        let containers: BTreeSet<PathBuf> = roots
            .iter()
            .filter(|root| {
                matches!(
                    root.kind,
                    RootKind::Storage | RootKind::HostCache | RootKind::HostState
                )
            })
            .map(|root| root.path.join("worktrees"))
            .filter(|container| is_real_directory(container))
            .collect();
        'containers: for container in containers {
            for name in names(&container) {
                if step.stopped() {
                    break 'containers;
                }
                if name.starts_with('.') {
                    continue;
                }
                let path = container.join(&name);
                if !is_real_directory(&path) {
                    continue;
                }
                let registered_live = live.contains(path.as_path());
                step.handle_leftover_root(&path, registered_live);
            }
        }
    }
    let report = step.report;
    crate::phase_telemetry::add(
        match report.stop.as_str() {
            "deadline" => "storage_artifact_age_stop_deadline",
            "lock_lost" => "storage_artifact_age_stop_lock_lost",
            _ => "storage_artifact_age_stop_done",
        },
        Duration::ZERO,
    );
    crate::phase_telemetry::attach(
        "storage_artifact_age",
        serde_json::to_value(&report).unwrap_or(serde_json::Value::Null),
    );
    Some(report)
}

#[cfg(test)]
mod tests;
