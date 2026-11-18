//! Strip catalogued regenerable directories out of closed checkouts, without
//! walking them, before anything judges what is left.
//!
//! Deciding whether a closed checkout may be deleted used to walk its whole
//! dependency tree through Git, several times, under a deadline: on a busy
//! repository one run judged four checkouts in 15 minutes and retired none,
//! while ten were created per hour. A prepared checkout is mostly
//! `node_modules` (1.6 of 2.4 GiB, ~127,000 files), which is regenerable by
//! definition. So it goes first, in O(1) per directory:
//!
//! 1. find the catalogued directories ([`StripKind`]: `node_modules`,
//!    `.pnpm-store`, `.venv`, Cargo `target`) by looking only where they
//!    live: the checkout's own top level, its sub-directories (a `backend/`
//!    or `packages/` holding a sub-project) and theirs, by directory listing
//!    and `lstat`, never entering a catalogued directory, `.git` or
//!    `.aethyme`. No Git walk: Git's ignored-file listing reads the whole
//!    source tree and took ~16 s per checkout on the busiest repository,
//!    so a pass finished almost nothing;
//! 2. prove them cheaply and without recursion, all of a checkout's
//!    candidates in two Git calls: `git check-ignore` on the directories
//!    themselves (ignored), `git ls-files -- <dirs>` naming no file under
//!    any (no tracked file: an index read, bounded output), every path
//!    component a real directory on the checkout's device (no symlink, no
//!    mount point), the producing tool's witness present and no nested
//!    repository at its top, and the checkout used by no process (the run's
//!    single open-file observation);
//! 3. rename it into the pool's existing trash with one exclusive
//!    same-volume rename, recorded first like a retired checkout; the paced
//!    host deleter unlinks it later. No archive: the content is regenerable.
//!
//! Safety invariants, each a refusal that keeps the directory where it is:
//!
//! - never a checkout of an open session, or one overlapping an open
//!   session's checkout, or one whose recorded holder is alive or unknown,
//!   or one a process has as its working directory or holds a file open in,
//!   or one with a lease, a running gate or an unresolved operation;
//! - never a directory that is tracked (or holds a tracked file), not
//!   ignored, a symlink (itself or any component above it), on another
//!   device, outside the catalogue, or missing its witness;
//! - never a path that may have been reused since it was judged: asked
//!   again under the lifecycle claim ([`Broker::reused_path_keep`]: another
//!   open session at the path, a row that judges the folder instead
//!   ([`path_superseded_by`]), a live holder, a folder born after the last
//!   close recorded there, a start holding the folder's reservation);
//!
//! The pass starts from folders that exist (one listing of each pool
//! directory, mapped to the sessions recorded there), judges each folder
//! once through the row that owns it, and counts the backlog only after it
//! stripped, so counting never spends the stripping budget.
//! - any check that fails or cannot complete keeps the directory;
//! - only same-volume renames; never a copy followed by a delete.
//!
//! Everything not in the catalogue is untouched and goes through the
//! existing careful retirement path, which then judges a small tree.

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// Checkouts the strip pass looks at per run, at most. Each costs a few
/// cheap Git reads and O(1) renames, so this is a cap, not a budget.
pub(crate) const STRIP_CHECKOUTS_PER_RUN: usize = 200;
/// Meta key: sessions whose checkout the strip pass already handled, with
/// the close time it saw (JSON: session id to `closed_at`), so the next run
/// does not list the same checkout again. A session closed again later (it
/// was reused) is looked at again.
pub(crate) const CHECKOUT_STRIP_DONE_KEY: &str = "checkout_strip.done";
/// Meta key: sessions whose checkout folder was proven absent (ENOENT, its
/// pool directory listable), with the close time seen (JSON: session id to
/// `closed_at`); dropped when the folder shows up again in its pool's
/// listing.
pub(crate) const CHECKOUT_STRIP_ABSENT_KEY: &str = "checkout_strip.absent";
/// Remembered handled sessions at most; the lowest ids go first.
const STRIP_DONE_MAX: usize = 16_384;
/// Meta key: checkouts whose directories were last kept only for a
/// transient reason (`probe_failed`, `deadline`, `move_failed`), with when
/// they may be looked at again (JSON: session id to [`StripBackoff`]).
pub(crate) const CHECKOUT_STRIP_BACKOFF_KEY: &str = "checkout_strip.backoff";
/// The first retry delay after a transient keep; it doubles per failure.
const STRIP_BACKOFF_BASE_MS: i64 = 5 * 60_000;
/// The longest retry delay.
const STRIP_BACKOFF_MAX_MS: i64 = 4 * 3_600_000;
/// Backed-off checkouts remembered at most; the earliest due go first.
const STRIP_BACKOFF_ENTRIES_MAX: usize = 4_096;
/// Names kept as a virtual environment's unexpected content, remembered for
/// `broker status` at most, and for how long after they were last seen.
const VENV_UNEXPECTED_NAMES_MAX: usize = 16;
const VENV_UNEXPECTED_KEPT_MS: i64 = 7 * 86_400_000;
/// Characters of one reported name, at most.
const REPORTED_NAME_MAX: usize = 128;
/// Closed or cleaned sessions read per strip pass, at most.
const STRIP_CANDIDATES_MAX: usize = 100_000;
/// Bytes at most read from one probe's output.
const STRIP_PROBE_OUTPUT_MAX: usize = 64 * 1024;
/// The longest one probe may take, under the caller's deadline.
const STRIP_PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// Days of daily counters kept.
const STRIP_DAYS_KEPT: usize = 14;
/// `broker status` advises once this many closed-session checkout folders
/// are still on disk.
pub(crate) const CLOSED_CHECKOUT_BACKLOG_ADVICE_AT: usize = 50;
const DAY_MS: i64 = 86_400_000;
/// Directory levels below the checkout whose catalogued children discovery
/// looks for: the top level (`node_modules`), a sub-project
/// (`backend/.venv`) and a workspace member (`apps/web/node_modules`). The
/// deepest level is probed by `lstat` of the catalogued names, not listed.
const DISCOVERY_DEPTH: usize = 2;
/// Directories discovery looks into per checkout, at most.
const DISCOVERY_DIRECTORIES_MAX: usize = 1_024;
/// Entries discovery reads from one directory listing, at most.
const DISCOVERY_ENTRIES_MAX: usize = 4_096;
/// Candidates proved per checkout, at most (two Git calls take them all).
const DISCOVERY_CANDIDATES_MAX: usize = 256;
/// Entries read from one pool directory listing before falling back to one
/// `lstat` per recorded folder.
const POOL_LISTING_MAX: usize = 200_000;
/// The longest the backlog count may take after the strip.
const BACKLOG_BUDGET: Duration = Duration::from_secs(5);
/// The longest the pool listings may take; past it each folder is checked
/// on its own, just before it is judged.
const POOL_LISTING_BUDGET: Duration = Duration::from_secs(10);
/// The longest one checkout may take, so a slow one never consumes the
/// pass (it is kept as `deadline` and backed off).
const STRIP_CHECKOUT_BUDGET: Duration = Duration::from_secs(15);
/// Checkouts a pass judges before it spends time on pool-wide accounting
/// (the `closed_with_regenerable` count), unless it ran out of candidates.
const STRIP_MIN_JUDGED_BEFORE_ACCOUNTING: usize = 10;
/// Meta key: where the last pass stopped (the judge's close time and
/// session id), so the next pass goes on from there, oldest close first,
/// wrapping at the end, instead of starting from the front again.
pub(crate) const CHECKOUT_STRIP_CURSOR_KEY: &str = "checkout_strip.cursor";
/// Directories the deepest discovery level probes per checkout, at most:
/// each probe costs `lstat`s, slow on a drive under deletion.
const DISCOVERY_DEEPEST_DIRECTORIES_MAX: usize = 32;

/// One checkout's retry backoff after transient keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct StripBackoff {
    /// The session's close time when it was kept: a checkout closed again
    /// (it changed) starts afresh.
    pub closed_at: i64,
    /// Consecutive passes that kept it for a transient reason.
    pub failures: u32,
    /// Not looked at again before this (epoch ms).
    pub until_ms: i64,
}

/// The retry delay after `failures` consecutive transient keeps: 5 minutes,
/// doubling, at most 4 hours.
pub(crate) fn strip_backoff_delay_ms(failures: u32) -> i64 {
    let doublings = failures.saturating_sub(1).min(20);
    STRIP_BACKOFF_BASE_MS
        .saturating_mul(1_i64 << doublings)
        .min(STRIP_BACKOFF_MAX_MS)
}

/// What one strip pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CheckoutStripReport {
    /// Closed-session checkout folders found on disk, one judge each.
    pub folders_found: usize,
    /// Judged paths whose folder is gone (by the pool listing).
    pub folders_missing: usize,
    /// Of those, skipped because an earlier pass handled the same close.
    pub skipped_remembered: usize,
    /// Of those, skipped because a transient keep's backoff has not passed
    /// (never counted toward the per-run cap).
    pub skipped_backoff: usize,
    /// Checkouts this pass kept only for a transient reason, now backed off.
    pub backoff_set: usize,
    /// Names that kept a virtual environment as `unexpected_content`
    /// (bounded; names only, never content).
    pub venv_unexpected: BTreeSet<String>,
    pub checkouts_examined: usize,
    /// Of those, checkouts that passed every precondition and were listed
    /// through Git; the per-run cap counts these, so checkouts refused
    /// before any Git work never starve newer ones.
    pub checkouts_listed: usize,
    pub checkouts_stripped: usize,
    /// Catalogued directories discovery found in the listed checkouts.
    pub candidates_found: usize,
    pub directories_stripped: usize,
    /// Refusals by reason code: a checkout-level code keeps the whole
    /// checkout, a directory-level one only that directory.
    pub refused: BTreeMap<String, usize>,
    /// The cap or the deadline stopped the pass with checkouts left.
    pub more_remaining: bool,
    /// The run's open-file observation could not be taken: nothing was
    /// stripped from the checkout that needed it, and the pass stopped.
    pub process_use_unavailable: bool,
    /// Milliseconds spent finding the folders, then discovering, proving
    /// and renaming.
    pub folder_listing_ms: u64,
    pub discover_ms: u64,
    pub prove_ms: u64,
    pub move_ms: u64,
    /// Checkout folders as the pass found them (counts only, no walk).
    pub backlog: CheckoutBacklog,
}

/// Checkout folders on disk, counted from the broker's records with one
/// `lstat` per folder and per top-level catalogued name: no byte walk.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CheckoutBacklog {
    /// Distinct session checkout folders that exist.
    pub folders: usize,
    /// Of those, folders whose sessions are all closed or cleaned and that
    /// no open session uses.
    pub closed_folders: usize,
    /// Of those, folders still holding a top-level catalogued regenerable
    /// directory (nested ones are not counted).
    pub closed_with_regenerable: usize,
    pub observed_at_ms: i64,
    /// The count of `closed_with_regenerable` ran out of its budget and
    /// covers only part of the closed folders.
    #[serde(default)]
    pub regenerable_partial: bool,
}

/// One day's comparable counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CheckoutStripDay {
    /// Sessions created that day (UTC): the checkouts that arrived.
    #[serde(default)]
    pub sessions_created: usize,
    /// Sessions whose close was recorded that day (UTC).
    pub sessions_closed: usize,
    /// Checkouts the retirement lane moved into the trash that day.
    pub checkouts_retired: usize,
    /// Of those, leftover folders of sessions already recorded as cleaned.
    #[serde(default)]
    pub cleaned_leftovers_retired: usize,
    pub checkouts_stripped: usize,
    pub directories_stripped: usize,
    /// Of the retired checkouts, those that went through a recovery archive
    /// that day, and the archives' bytes.
    #[serde(default)]
    pub checkouts_archived: usize,
    #[serde(default)]
    pub archive_bytes: u64,
    /// Bytes of stripped directories the deleter removed that day.
    pub bytes_freed: u64,
}

/// Strip progress kept across runs, for the storage ledger and `status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CheckoutStripProgress {
    pub last_checkouts_examined: usize,
    pub last_checkouts_stripped: usize,
    pub last_directories_stripped: usize,
    #[serde(default)]
    pub last_refused: BTreeMap<String, usize>,
    #[serde(default)]
    pub last_phase_ms: BTreeMap<String, u64>,
    #[serde(default)]
    pub backlog: CheckoutBacklog,
    /// Bytes of stripped directories the deleter removed, all runs.
    #[serde(default)]
    pub bytes_freed_total: u64,
    /// Daily counters by UTC day number (epoch ms / 86,400,000).
    #[serde(default)]
    pub days: BTreeMap<i64, CheckoutStripDay>,
    /// Names that kept a virtual environment as `unexpected_content`, with
    /// when they were last seen (epoch ms), for 7 days; a name the
    /// repository has since declared under `[cleanup] venv_markers` drops.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub venv_unexpected: BTreeMap<String, i64>,
}

/// A catalogued directory found in a checkout, not yet proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StripCandidate {
    /// Relative to the checkout, slash-separated, no trailing slash.
    pub relative: String,
    pub kind: StripKind,
}

/// The directories the strip may move, each with the witness that must be
/// present for it to count as that tool's output. Deliberately narrower than
/// the unattended artifact catalogue: a strip is unattended and removes the
/// whole directory, so every witness fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StripKind {
    /// `node_modules`: a package manager's own marker at its top
    /// (`.modules.yaml`, `.package-lock.json`, `.yarn-integrity`) or pnpm's
    /// `.pnpm` directory.
    NodeModules,
    /// `.pnpm-store`: pnpm's store layout, a `v<digit>...` directory.
    PnpmStore,
    /// `.venv`: `pyvenv.cfg`, and nothing at its top outside the usual
    /// virtual-environment layout.
    Venv,
    /// Cargo `target`: `CACHEDIR.TAG` carrying the standard signature.
    CargoTarget,
}

/// Top-level entries a virtual environment may hold.
const VENV_LAYOUT: &[&str] = &[
    "bin",
    "lib",
    "lib64",
    "include",
    "share",
    "etc",
    "pyvenv.cfg",
    ".gitignore",
    "Scripts",
    "man",
];

/// Top-level entries read at most to judge a witness.
const WITNESS_ENTRY_LIMIT: usize = 4_096;

impl StripKind {
    pub(crate) fn for_name(name: &str) -> Option<Self> {
        match name {
            "node_modules" => Some(Self::NodeModules),
            ".pnpm-store" => Some(Self::PnpmStore),
            ".venv" => Some(Self::Venv),
            "target" => Some(Self::CargoTarget),
            _ => None,
        }
    }

    /// The phase counter of one directory of this kind found by discovery.
    fn found_counter(self) -> &'static str {
        match self {
            Self::NodeModules => "checkout_strip_found_node_modules",
            Self::PnpmStore => "checkout_strip_found_pnpm_store",
            Self::Venv => "checkout_strip_found_venv",
            Self::CargoTarget => "checkout_strip_found_cargo_target",
        }
    }

    /// The phase counter of one directory of this kind moved to the trash.
    fn stripped_counter(self) -> &'static str {
        match self {
            Self::NodeModules => "checkout_strip_stripped_node_modules",
            Self::PnpmStore => "checkout_strip_stripped_pnpm_store",
            Self::Venv => "checkout_strip_stripped_venv",
            Self::CargoTarget => "checkout_strip_stripped_cargo_target",
        }
    }

    /// Whether `path` (a real directory) is this tool's output. `Err` is
    /// the keep reason: `witness_missing`, `unexpected_content`, or
    /// `nested_repository` when a `.git` entry sits at its top (a nested
    /// repository or submodule is never regenerable). Reads only the
    /// directory's own top level (bounded) and at most one small file.
    /// `venv_markers` are the file names the repository declares under
    /// `[cleanup] venv_markers` (see [`venv_unexpected_names`]).
    pub(crate) fn witness(self, path: &Path, venv_markers: &[String]) -> Result<(), &'static str> {
        match std::fs::symlink_metadata(path.join(".git")) {
            Ok(_) => return Err("nested_repository"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("witness_missing"),
        }
        let is_file = |name: &str| {
            std::fs::symlink_metadata(path.join(name)).is_ok_and(|metadata| metadata.is_file())
        };
        let is_dir = |name: &str| {
            std::fs::symlink_metadata(path.join(name)).is_ok_and(|metadata| metadata.is_dir())
        };
        let top_names = || top_names(path);
        match self {
            Self::NodeModules => {
                if [".modules.yaml", ".package-lock.json", ".yarn-integrity"]
                    .iter()
                    .any(|&name| is_file(name))
                    || is_dir(".pnpm")
                {
                    Ok(())
                } else {
                    Err("witness_missing")
                }
            }
            Self::PnpmStore => {
                let layout = top_names()?.iter().any(|name| {
                    name.strip_prefix('v')
                        .and_then(|rest| rest.chars().next())
                        .is_some_and(|first| first.is_ascii_digit())
                        && is_dir(name.as_str())
                });
                if layout {
                    Ok(())
                } else {
                    Err("witness_missing")
                }
            }
            Self::Venv => {
                if !is_file("pyvenv.cfg") {
                    return Err("witness_missing");
                }
                if venv_unexpected_names(path, venv_markers)?.is_empty() {
                    Ok(())
                } else {
                    Err("unexpected_content")
                }
            }
            Self::CargoTarget => {
                let mut signature = [0_u8; 43];
                let read = std::fs::File::open(path.join("CACHEDIR.TAG"))
                    .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut signature));
                if is_file("CACHEDIR.TAG") && read.is_ok() && signature == *CACHEDIR_TAG_SIGNATURE {
                    Ok(())
                } else {
                    Err("witness_missing")
                }
            }
        }
    }
}

/// The top-level entry names of `path`, bounded. `Err` is the keep reason.
fn top_names(path: &Path) -> Result<Vec<String>, &'static str> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|_| "witness_missing")? {
        let entry = entry.map_err(|_| "witness_missing")?;
        names.push(
            entry
                .file_name()
                .into_string()
                .map_err(|_| "unexpected_content")?,
        );
        if names.len() > WITNESS_ENTRY_LIMIT {
            return Err("unexpected_content");
        }
    }
    Ok(names)
}

/// Files Python environment tools themselves write at a virtual
/// environment's top beside the layout (uv's cache tag and lock).
const VENV_TOOL_MARKERS: &[&str] = &["CACHEDIR.TAG", ".lock"];

/// The entries at the top of the virtual environment `path` that are
/// neither its layout ([`VENV_LAYOUT`]) nor a regular file named by
/// [`VENV_TOOL_MARKERS`] or by the repository's `[cleanup] venv_markers`
/// (`declared`): what keeps it as `unexpected_content`. Fails closed: any
/// other file, a dotfile such as `.env` included, or any other directory is
/// someone's content, not the tool's output. Sorted.
pub(crate) fn venv_unexpected_names(
    path: &Path,
    declared: &[String],
) -> Result<Vec<String>, &'static str> {
    let mut unexpected = top_names(path)?
        .into_iter()
        .filter(|name| {
            let marker = VENV_TOOL_MARKERS.contains(&name.as_str())
                || declared.iter().any(|declared| declared == name);
            let accepted = VENV_LAYOUT.contains(&name.as_str())
                || (marker
                    && std::fs::symlink_metadata(path.join(name))
                        .is_ok_and(|metadata| metadata.is_file()));
            !accepted
        })
        .collect::<Vec<_>>();
    unexpected.sort();
    Ok(unexpected)
}

/// The first bytes of a `CACHEDIR.TAG` under the Cache Directory Tagging
/// specification.
const CACHEDIR_TAG_SIGNATURE: &[u8; 43] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// The catalogued regenerable directories of `worktree`, found where they
/// live without asking Git to walk anything: the checkout's top level and
/// its sub-directories are listed, and the level below those
/// ([`DISCOVERY_DEPTH`]) is probed with one `lstat` per catalogued name. A
/// catalogued directory, `.git`, `.aethyme` and broker runtime paths are
/// never entered, symlinks never followed, and every listing is bounded, so
/// discovery costs a few hundred directory reads at most however large the
/// tree. Whether a candidate is ignored, untracked and its tool's output is
/// judged by [`prove_strippable_all`], which records why one is kept.
///
/// The checkout must carry its own `.git` entry, so every Git call the
/// proof makes from it resolves to this checkout and no enclosing one.
pub(crate) fn regenerable_candidates(
    worktree: &Path,
    deadline: Instant,
) -> Result<Vec<StripCandidate>, String> {
    let _span = crate::phase_telemetry::span("checkout_strip_discover");
    match std::fs::symlink_metadata(worktree.join(".git")) {
        Ok(metadata) if metadata.is_file() || metadata.is_dir() => {}
        Ok(_) => return Err("the checkout's .git entry is neither a file nor a directory".into()),
        Err(error) => return Err(format!("the checkout has no .git entry: {error}")),
    }
    let mut candidates = Vec::new();
    let mut seen_directories = 0_usize;
    let mut deepest_probed = 0_usize;
    // (absolute directory, its path relative to the checkout)
    let mut level: Vec<(PathBuf, String)> = vec![(worktree.to_path_buf(), String::new())];
    'discovery: for depth in 0..=DISCOVERY_DEPTH {
        let mut next = Vec::new();
        for (directory, relative) in level {
            if Instant::now() >= deadline {
                return Err("discovery ran out of time".into());
            }
            if seen_directories >= DISCOVERY_DIRECTORIES_MAX
                || candidates.len() >= DISCOVERY_CANDIDATES_MAX
            {
                crate::phase_telemetry::add("checkout_strip_discover_bounded", Duration::ZERO);
                break 'discovery;
            }
            seen_directories += 1;
            let join = |name: &str| {
                if relative.is_empty() {
                    name.to_string()
                } else {
                    format!("{relative}/{name}")
                }
            };
            if depth == DISCOVERY_DEPTH {
                // The deepest level: probe the catalogued names only, in a
                // bounded number of directories.
                if deepest_probed >= DISCOVERY_DEEPEST_DIRECTORIES_MAX {
                    crate::phase_telemetry::add("checkout_strip_discover_bounded", Duration::ZERO);
                    continue;
                }
                deepest_probed += 1;
                // Only `node_modules` (workspace members): one `lstat` per
                // directory, each ~120 ms on a drive under deletion.
                let child = join("node_modules");
                if !crate::runtime_paths::is_broker_runtime_path(&child)
                    && std::fs::symlink_metadata(directory.join("node_modules"))
                        .is_ok_and(|metadata| metadata.is_dir())
                {
                    candidates.push(StripCandidate {
                        relative: child,
                        kind: StripKind::NodeModules,
                    });
                }
                continue;
            }
            let entries = match std::fs::read_dir(&directory) {
                Ok(entries) => entries,
                // The checkout itself must be readable; a sub-directory that
                // is not is only one place less to look.
                Err(error) if depth == 0 => {
                    return Err(format!("cannot list the checkout: {error}"));
                }
                Err(_) => continue,
            };
            for (count, entry) in entries.enumerate() {
                if count >= DISCOVERY_ENTRIES_MAX {
                    crate::phase_telemetry::add("checkout_strip_discover_bounded", Duration::ZERO);
                    break;
                }
                let Ok(entry) = entry else { continue };
                // `file_type` does not follow a symlink: a link is skipped.
                if !entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
                    continue;
                }
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if name == ".git" || name == ".aethyme" {
                    continue;
                }
                let child = join(&name);
                if crate::runtime_paths::is_broker_runtime_path(&child) {
                    continue;
                }
                if let Some(kind) = StripKind::for_name(&name) {
                    candidates.push(StripCandidate {
                        relative: child,
                        kind,
                    });
                    continue;
                }
                next.push((entry.path(), child));
            }
        }
        level = next;
    }
    candidates.sort_by(|left, right| left.relative.cmp(&right.relative));
    candidates.truncate(DISCOVERY_CANDIDATES_MAX);
    Ok(candidates)
}

#[cfg(test)]
thread_local! {
    /// Makes every strip probe fail, as a Git that cannot complete would.
    static PROBES_FAIL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn set_probes_fail_for_tests(fail: bool) {
    PROBES_FAIL.with(|cell| cell.set(fail));
}

/// Run one read-only Git probe in `worktree` under `deadline`; returns its
/// exit code and bounded stdout. Anything but a completed run is an error.
fn git_probe(worktree: &Path, args: &[&str], deadline: Instant) -> Result<(i32, Vec<u8>), String> {
    #[cfg(test)]
    if PROBES_FAIL.with(std::cell::Cell::get) {
        return Err(format!("git {}: failed for the test", args.join(" ")));
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(format!("git {}: out of time", args.join(" ")));
    }
    let mut command = crate::git::readonly_listing_command(worktree, args);
    match crate::bounded_output::output_limited(
        &mut command,
        remaining
            .min(STRIP_PROBE_TIMEOUT)
            .min(crate::git::git_timeout()),
        STRIP_PROBE_OUTPUT_MAX,
    ) {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) => {
            Ok((output.status.code().unwrap_or(-1), output.stdout))
        }
        Ok(crate::bounded_output::LimitedOutput::Deadline) => {
            Err(format!("git {}: out of time", args.join(" ")))
        }
        Ok(crate::bounded_output::LimitedOutput::OutputLimit) => {
            Err(format!("git {}: output over its limit", args.join(" ")))
        }
        Err(error) => Err(format!("git {}: {error}", args.join(" "))),
    }
}

/// Prove `candidate` may be stripped; see [`prove_strippable_all`].
#[cfg(test)]
pub(crate) fn prove_strippable(
    worktree: &Path,
    candidate: &StripCandidate,
    deadline: Instant,
) -> Result<(), &'static str> {
    prove_strippable_all(worktree, std::slice::from_ref(candidate), deadline, &[])
        .pop()
        .unwrap_or(Err("probe_failed"))
}

/// The on-disk half of the proof: every component a real directory (no
/// symlink), on the checkout's device, and the tool's witness present.
fn prove_on_disk(
    worktree: &Path,
    candidate: &StripCandidate,
    venv_markers: &[String],
) -> Result<(), &'static str> {
    use std::os::unix::fs::MetadataExt;
    let checkout = std::fs::symlink_metadata(worktree).map_err(|_| "checkout_missing")?;
    if !checkout.is_dir() {
        return Err("checkout_missing");
    }
    let mut path = worktree.to_path_buf();
    for part in candidate.relative.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err("not_a_real_directory");
        }
        path.push(part);
        let metadata = std::fs::symlink_metadata(&path).map_err(|_| "not_a_real_directory")?;
        if metadata.file_type().is_symlink() {
            return Err("symlink");
        }
        if !metadata.is_dir() {
            return Err("not_a_real_directory");
        }
        if metadata.dev() != checkout.dev() {
            return Err("crosses_device");
        }
    }
    match candidate.kind.witness(&path, venv_markers) {
        // A workspace member's `node_modules` (never the checkout's own
        // top-level one) carries no package-manager marker of its own:
        // pnpm fills it with links into the top-level store.
        Err("witness_missing")
            if candidate.kind == StripKind::NodeModules
                && candidate.relative.contains('/')
                && pnpm_member_node_modules(&path, &worktree.join("node_modules/.bin")) =>
        {
            crate::phase_telemetry::add("checkout_strip_witness_pnpm_member", Duration::ZERO);
            Ok(())
        }
        verdict => verdict,
    }
}

/// Bytes a `.bin` shim may hold, at most.
const PNPM_BIN_SHIM_MAX_BYTES: u64 = 64 * 1024;

/// Whether `path` is a pnpm workspace member's `node_modules` and nothing
/// else (fail closed, top level and one level down only, bounded):
///
/// - every entry is a symbolic link, a `.bin` directory, or an `@scope`
///   directory;
/// - `.bin` holds only symbolic links, or regular files that start with
///   `#!` (pnpm's shell shims), are at most 64 KiB and whose name is also
///   in the checkout's top-level `node_modules/.bin` (`top_bin`), where pnpm
///   writes the same shims;
/// - an `@scope` directory holds only symbolic links;
/// - at least one link points into a pnpm store (`node_modules/.pnpm/`).
///
/// Links are never followed; renaming the directory moves the links, not
/// what they point at. Anything else, a cache directory such as `.vite`
/// included, keeps it as `witness_missing`.
pub(crate) fn pnpm_member_node_modules(path: &Path, top_bin: &Path) -> bool {
    let read = |directory: &Path| -> Option<Vec<std::fs::DirEntry>> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(directory).ok()? {
            entries.push(entry.ok()?);
            if entries.len() > WITNESS_ENTRY_LIMIT {
                return None;
            }
        }
        Some(entries)
    };
    let mut store_link = false;
    let mut link_into_store = |entry: &std::fs::DirEntry| {
        if std::fs::read_link(entry.path()).is_ok_and(|target| {
            let target = target.to_string_lossy();
            target.contains("node_modules/.pnpm/") || target.starts_with(".pnpm/")
        }) {
            store_link = true;
        }
    };
    let Some(entries) = read(path) else {
        return false;
    };
    for entry in &entries {
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_symlink() {
            link_into_store(entry);
        } else if file_type.is_dir() && name == ".bin" {
            let Some(shims) = read(&entry.path()) else {
                return false;
            };
            for shim in &shims {
                let Ok(shim_type) = shim.file_type() else {
                    return false;
                };
                if shim_type.is_symlink() {
                    continue;
                }
                let is_shim = shim_type.is_file()
                    && std::fs::symlink_metadata(top_bin.join(shim.file_name())).is_ok()
                    && shim
                        .metadata()
                        .is_ok_and(|metadata| metadata.len() <= PNPM_BIN_SHIM_MAX_BYTES)
                    && std::fs::File::open(shim.path())
                        .and_then(|mut file| {
                            let mut start = [0_u8; 2];
                            std::io::Read::read_exact(&mut file, &mut start).map(|()| start)
                        })
                        .is_ok_and(|start| &start == b"#!");
                if !is_shim {
                    return false;
                }
            }
        } else if file_type.is_dir() && name.starts_with('@') && name.len() > 1 {
            let Some(links) = read(&entry.path()) else {
                return false;
            };
            for link in &links {
                if !link.file_type().is_ok_and(|kind| kind.is_symlink()) {
                    return false;
                }
                link_into_store(link);
            }
        } else {
            return false;
        }
    }
    store_link
}

/// Prove each of `candidates` may be stripped, cheaply and without
/// recursion: on disk ([`prove_on_disk`]), then Git calling the directory
/// itself ignored and the index holding no file under it, both asked for
/// every remaining candidate at once (two Git calls per checkout, however
/// many candidates). One verdict per candidate, in order; `Err` is the
/// refusal code, and any probe that fails refuses every candidate it was
/// asked about.
pub(crate) fn prove_strippable_all(
    worktree: &Path,
    candidates: &[StripCandidate],
    deadline: Instant,
    venv_markers: &[String],
) -> Vec<Result<(), &'static str>> {
    let _span = crate::phase_telemetry::span("checkout_strip_prove");
    let mut verdicts = candidates
        .iter()
        .map(|candidate| prove_on_disk(worktree, candidate, venv_markers))
        .collect::<Vec<_>>();
    let pending = |verdicts: &[Result<(), &'static str>]| {
        verdicts
            .iter()
            .enumerate()
            .filter(|(_, verdict)| verdict.is_ok())
            .map(|(index, _)| index)
            .collect::<Vec<_>>()
    };
    let records = |stdout: &[u8]| {
        stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>()
    };
    let asked = pending(verdicts.as_slice());
    if asked.is_empty() {
        return verdicts;
    }
    // The directories themselves, not their contents: Git prints each
    // ignored one it was given, one per line (exit 0), and exits 1 when
    // none is. `-z` needs `--stdin`, so paths are read as lines, unquoted
    // by `core.quotePath=false`; a name Git still quotes (a control
    // character, a quote or a backslash) matches nothing and is kept.
    // `check-ignore` takes plain paths and refuses pathspec magic, so no
    // `--literal-pathspecs` here.
    let mut args = vec![
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.quotePath=false",
        "check-ignore",
        "--",
    ];
    args.extend(
        asked
            .iter()
            .map(|&index| candidates[index].relative.as_str()),
    );
    match git_probe(worktree, &args, deadline) {
        Ok((code @ (0 | 1), stdout)) => {
            let ignored = if code == 0 {
                stdout
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                    .map(<[u8]>::to_vec)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            for &index in &asked {
                if !ignored
                    .iter()
                    .any(|record| record.as_slice() == candidates[index].relative.as_bytes())
                {
                    verdicts[index] = Err("not_ignored");
                }
            }
        }
        Ok(_) | Err(_) => {
            for &index in &asked {
                verdicts[index] = Err("probe_failed");
            }
            return verdicts;
        }
    }
    let asked = pending(verdicts.as_slice());
    if asked.is_empty() {
        return verdicts;
    }
    // An index read with literal pathspecs: any file named is tracked and
    // under the candidate it names. Never reads the working tree.
    let mut args = vec![
        "--literal-pathspecs",
        "-c",
        "core.fsmonitor=false",
        "ls-files",
        "-z",
        "--",
    ];
    args.extend(
        asked
            .iter()
            .map(|&index| candidates[index].relative.as_str()),
    );
    match git_probe(worktree, &args, deadline) {
        Ok((0, stdout)) => {
            let tracked = records(stdout.as_slice());
            for &index in &asked {
                let directory = candidates[index].relative.as_bytes();
                if tracked.iter().any(|path| {
                    path.as_slice() == directory
                        || (path.starts_with(directory) && path.get(directory.len()) == Some(&b'/'))
                }) {
                    verdicts[index] = Err("holds_tracked");
                }
            }
        }
        Ok(_) | Err(_) => {
            for &index in &asked {
                verdicts[index] = Err("probe_failed");
            }
        }
    }
    verdicts
}

/// The phase counter of one strip refusal, by code.
fn refused_counter(code: &str) -> &'static str {
    match code {
        "not_closed" => "checkout_strip_refused_not_closed",
        "adopted" => "checkout_strip_refused_adopted",
        "checkout_missing" => "checkout_strip_refused_checkout_missing",
        "tombstone" => "checkout_strip_refused_tombstone",
        "checkout_unreadable" => "checkout_strip_refused_checkout_unreadable",
        "not_owned" => "checkout_strip_refused_not_owned",
        "open_session" => "checkout_strip_refused_open_session",
        "open_session_unknown" => "checkout_strip_refused_open_session_unknown",
        "lease_held" => "checkout_strip_refused_lease_held",
        "lease_unknown" => "checkout_strip_refused_lease_unknown",
        "gate_running" => "checkout_strip_refused_gate_running",
        "gates_unknown" => "checkout_strip_refused_gates_unknown",
        "operation_unresolved" => "checkout_strip_refused_operation_unresolved",
        "operations_unknown" => "checkout_strip_refused_operations_unknown",
        "holder_alive" => "checkout_strip_refused_holder_alive",
        "holder_unknown" => "checkout_strip_refused_holder_unknown",
        "process_using_checkout" => "checkout_strip_refused_process_using_checkout",
        "process_use_unknown" => "checkout_strip_refused_process_use_unknown",
        "discovery_failed" => "checkout_strip_refused_discovery_failed",
        "lifecycle_busy" => "checkout_strip_refused_lifecycle_busy",
        "changed" => "checkout_strip_refused_changed",
        "not_a_real_directory" => "checkout_strip_refused_not_a_real_directory",
        "symlink" => "checkout_strip_refused_symlink",
        "crosses_device" => "checkout_strip_refused_crosses_device",
        "witness_missing" => "checkout_strip_refused_witness_missing",
        "unexpected_content" => "checkout_strip_refused_unexpected_content",
        "nested_repository" => "checkout_strip_refused_nested_repository",
        "start_reserved" => "checkout_strip_refused_start_reserved",
        "start_reservation_unknown" => "checkout_strip_refused_start_reservation_unknown",
        "created_after_close" => "checkout_strip_refused_created_after_close",
        "creation_time_unknown" => "checkout_strip_refused_creation_time_unknown",
        "leftover_superseded" => "checkout_strip_refused_leftover_superseded",
        "not_ignored" => "checkout_strip_refused_not_ignored",
        "holds_tracked" => "checkout_strip_refused_holds_tracked",
        "probe_failed" => "checkout_strip_refused_probe_failed",
        "move_failed" => "checkout_strip_refused_move_failed",
        "deadline" => "checkout_strip_refused_deadline",
        _ => "checkout_strip_refused_other",
    }
}

/// Directory refusals that say nothing lasting about the directory.
fn transient_refusals(report: &CheckoutStripReport) -> usize {
    ["probe_failed", "deadline", "move_failed"]
        .iter()
        .filter_map(|code| report.refused.get(*code))
        .sum()
}

fn note_refusal(report: &mut CheckoutStripReport, code: &'static str) {
    *report.refused.entry(code.to_string()).or_default() += 1;
    crate::phase_telemetry::add(refused_counter(code), Duration::ZERO);
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// What `broker start`'s pressure pass did.
pub(crate) struct PressureRelief {
    pub report: CheckoutStripReport,
    /// Bytes the deleter freed.
    pub freed: u64,
    pub strip_ms: u64,
    pub drain_ms: u64,
}

/// One checkout's outcome.
enum CheckoutStrip {
    /// Stripped this many directories (zero: nothing catalogued was left).
    Done(usize),
    /// Kept whole, by code.
    Refused(&'static str),
}

/// How long after a session's creation its checkout folder may still be
/// born without reading as recreated (see `reused_path_keep`).
const CREATION_SLACK_MS: i64 = 60_000;

/// When the checkout folder was created (epoch ms): the directory's birth
/// time where the filesystem records one, otherwise the status-change time
/// of its `.git` file (written once when Git creates the worktree; the
/// directory's own change time moves whenever an entry is added).
pub(crate) fn checkout_created_ms(worktree: &Path) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    let to_ms = |time: std::time::SystemTime| {
        time.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|since| i64::try_from(since.as_millis()).ok())
    };
    if let Some(born) = std::fs::symlink_metadata(worktree)
        .ok()
        .and_then(|metadata| metadata.created().ok())
        .and_then(to_ms)
    {
        return Some(born);
    }
    let dot_git = std::fs::symlink_metadata(worktree.join(".git")).ok()?;
    dot_git
        .ctime()
        .checked_mul(1_000)
        .map(|ms| ms + dot_git.ctime_nsec() / 1_000_000)
}

/// Whether `session` is not the row that judges the folder at its path,
/// given every row recorded for that path (`rows`, `session` among them):
///
/// - a session recorded as cleaned judges a leftover folder only when every
///   row at the path is cleaned too; any closed or open row there owns the
///   folder (it may hold that session's work), so the cleaned one is
///   superseded and that row's own judgement decides;
/// - a closed session is superseded only by a newer row that is not
///   cleaned: the newest closed row judges, and a cleaned row never takes a
///   folder from a closed one.
///
/// So every folder whose rows are all closed or cleaned has exactly one
/// judge that is never superseded, and a path reused by many cleaned rows
/// (paths derived from task text are) is retired through any of them.
pub(crate) fn path_superseded_by(session: &Session, rows: &[Session]) -> bool {
    superseder_of(session, rows).is_some()
}

/// The row that supersedes `session` at its path under
/// [`path_superseded_by`] (the newest such), if any.
pub(crate) fn superseder_of(session: &Session, rows: &[Session]) -> Option<i64> {
    rows.iter()
        .filter(|row| row.cleanup_state != crate::SessionCleanupState::Cleaned)
        .filter(|row| {
            session.cleanup_state == crate::SessionCleanupState::Cleaned || row.id > session.id
        })
        .map(|row| row.id)
        .max()
}

/// The row that judges a folder, of the closed or cleaned rows recorded
/// for its path: the newest closed one, else the newest (all are cleaned).
/// The one row [`path_superseded_by`] never supersedes.
fn judges_before(row: &Session, current: &Session) -> bool {
    let rank = |session: &Session| {
        (
            session.cleanup_state != crate::SessionCleanupState::Cleaned,
            session.id,
        )
    };
    rank(row) > rank(current)
}

/// What one checkout's strip is judged with besides the run's evidence.
#[derive(Clone, Copy)]
struct StripContext<'a> {
    /// The repository's `[cleanup] venv_markers`.
    venv_markers: &'a [String],
    /// At `finish`, the agent finishing the session: its own holder record
    /// does not keep the checkout.
    finishing_caller: Option<&'a crate::session_holder::AgentProcess>,
}

/// A judge's place in the pass: its close time, then its id.
fn judge_key(session: &Session) -> (i64, i64) {
    (
        session
            .closed_at
            .or(session.cleanup_completed_at)
            .unwrap_or(session.updated_at),
        session.id,
    )
}

/// Folder existence from one listing of each pool directory: the busiest
/// repository records ~7,600 sessions for ~850 folders, and on its external
/// drive, while the trash deleter runs, every metadata call takes ~120 ms.
/// The listing's own entry types (no `lstat` per name) say which names are
/// real directories; 0.8.122 confirmed each listed name with an `lstat`,
/// which took 86.7 s for ~720 folders and starved the whole pass. A pool
/// directory that cannot be listed completely before `deadline` answers
/// `None` (unknown), and the caller checks that folder itself.
#[derive(Default)]
pub(super) struct PoolListing {
    parents: BTreeMap<PathBuf, Option<BTreeSet<std::ffi::OsString>>>,
}

impl PoolListing {
    /// Whether `path` is a real directory by its pool directory's listing,
    /// `None` when that listing is not available.
    pub(super) fn exists(&mut self, path: &str, deadline: Instant) -> Option<bool> {
        let folder = Path::new(path);
        let (parent, name) = (folder.parent()?, folder.file_name()?);
        self.parents
            .entry(parent.to_path_buf())
            .or_insert_with(|| {
                let _span = crate::phase_telemetry::span("checkout_strip_folder_listing");
                // Each pool directory gets its own budget, from when it is
                // first needed: a listing that starts late in the pass is
                // not cut short by the time the pass already spent.
                let listed = list_directory_names(
                    parent,
                    deadline.min(Instant::now() + POOL_LISTING_BUDGET),
                );
                if listed.is_none() {
                    crate::phase_telemetry::add(
                        "checkout_strip_folder_listing_unavailable",
                        Duration::ZERO,
                    );
                }
                listed
            })
            .as_ref()
            .map(|names| names.contains(name))
    }
}

/// The names of the real directories (not symlinks) in `directory`, from
/// the listing's entry types, or `None` when it cannot be listed completely
/// within [`POOL_LISTING_MAX`] entries before `deadline`. A pool directory
/// that does not exist holds no folder (an empty set: 7 of the busiest
/// repository's 12 recorded pool directories are gone, and 0.8.123 read each
/// as unknown and checked its folders one by one). An entry that vanishes
/// while it is listed (the deleter and the reclaim-plan files churn at the
/// pool root) is skipped, not a reason to drop the whole listing: a folder
/// it hides is only judged a pass later.
fn list_directory_names(
    directory: &Path,
    deadline: Instant,
) -> Option<BTreeSet<std::ffi::OsString>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::phase_telemetry::add("checkout_strip_pool_directory_absent", Duration::ZERO);
            return Some(BTreeSet::new());
        }
        Err(_) => return None,
    };
    let mut names = BTreeSet::new();
    for (count, entry) in entries.enumerate() {
        if count >= POOL_LISTING_MAX || Instant::now() >= deadline {
            return None;
        }
        let Ok(entry) = entry else {
            continue;
        };
        if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            names.insert(entry.file_name());
        }
    }
    Some(names)
}

#[cfg(test)]
thread_local! {
    /// Runs when the backlog count starts, after the strip.
    static BEFORE_BACKLOG: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn set_before_backlog_hook(hook: Option<Box<dyn FnMut()>>) {
    BEFORE_BACKLOG.with(|cell| *cell.borrow_mut() = hook);
}

impl Broker {
    /// What a closed or cleaned session's recorded holder says about
    /// removing anything from its checkout: `None` when it does not keep it.
    /// Gone or never bound does not keep it; an unknown listing keeps it
    /// (`holder_unknown`); a live holder (same pid and start time) keeps it
    /// (`holder_alive`) unless it has since been bound to a newer session
    /// after this one's last activity and close: one long-lived agent
    /// process routinely starts many sessions in turn, and on the busiest
    /// repository 202 of the 207 closed or cleaned sessions whose holder
    /// was still running had that holder bound to a newer session. The same
    /// moved-on rule the stale lane applies (`holder_moved_on`). A process
    /// still in the checkout (working directory or open file) keeps it by
    /// the separate open-file check, which this never replaces.
    pub(crate) fn closed_checkout_holder_keep(
        &self,
        session: &Session,
        holders: &mut crate::session_holder::RecordedHolderProcessTable,
    ) -> Option<&'static str> {
        use crate::session_holder::RecordedHolderLiveness;
        let assessment = crate::session_holder::recorded_holder_liveness(
            self.store_ref(),
            session.id,
            holders,
            true,
        );
        match assessment.liveness {
            RecordedHolderLiveness::Unbound | RecordedHolderLiveness::Gone => None,
            RecordedHolderLiveness::Unknown => Some("holder_unknown"),
            RecordedHolderLiveness::Alive => {
                let activity = session
                    .last_activity_at
                    .max(session.closed_at.unwrap_or(0))
                    .max(session.cleanup_completed_at.unwrap_or(0));
                let moved_on = match self.store_ref().latest_session_holder_event(session.id) {
                    Ok(Some(event)) if Some(event.id) == assessment.binding_event_id => self
                        .holder_moved_on(session.id, &event, activity)
                        .ok()
                        .flatten(),
                    _ => None,
                };
                if moved_on.is_some() {
                    crate::phase_telemetry::add("checkout_holder_moved_on", Duration::ZERO);
                    None
                } else {
                    Some("holder_alive")
                }
            }
        }
    }

    /// Whether `session` is not the row that judges its path's folder
    /// ([`path_superseded_by`] over every row recorded for the path).
    pub(crate) fn path_superseded(&self, session: &Session) -> Result<bool, BrokerError> {
        let rows = self
            .store_ref()
            .sessions_at_worktree_path(&session.worktree_path)?;
        Ok(path_superseded_by(session, &rows))
    }

    /// What keeps `session`'s checkout `worktree`, checked under the
    /// lifecycle claim right before anything is moved out of it, because a
    /// `start --reuse` or a new session may have reopened or recreated the
    /// path since the checkout was judged: another open session at the
    /// path, another row that judges the folder ([`path_superseded_by`]), a
    /// live or unknown holder (when `check_holder`), a folder created after
    /// the last close recorded at the path, or a start holding the folder's
    /// reservation. `None` lets the move go on.
    pub(crate) fn reused_path_keep(
        &self,
        session: &Session,
        worktree: &Path,
        check_holder: bool,
    ) -> Option<&'static str> {
        let _span = crate::phase_telemetry::span("checkout_reused_path_check");
        match self
            .store_ref()
            .other_open_session_at_path(session.id, &session.worktree_path)
        {
            Ok(false) => {}
            Ok(true) => return Some("open_session"),
            Err(_) => return Some("open_session_unknown"),
        }
        let rows = match self
            .store_ref()
            .sessions_at_worktree_path(&session.worktree_path)
        {
            Ok(rows) => rows,
            Err(_) => return Some("open_session_unknown"),
        };
        if path_superseded_by(session, &rows) {
            return Some("leftover_superseded");
        }
        if check_holder {
            let mut holders = crate::session_holder::RecordedHolderProcessTable::default();
            if let Some(code) = self.closed_checkout_holder_keep(session, &mut holders) {
                return Some(code);
            }
        }
        let Some(closed_at) = session.closed_at.or(session.cleanup_completed_at) else {
            return Some("creation_time_unknown");
        };
        // Every row at the path is closed or cleaned (an open one kept the
        // folder above), and the folder is judged through one of them: it
        // may have been created for any, so the last close recorded there
        // is the bound.
        let closed_at = rows
            .iter()
            .filter_map(|row| row.closed_at.or(row.cleanup_completed_at))
            .fold(closed_at, i64::max);
        // A folder born after the close is not the one the session left.
        // The reference never precedes a minute after the session's own
        // creation: Git creates the folder around the time the session row
        // is written, and a record whose close predates its creation (a
        // clock step, a rewritten row) must not read as a recreated folder.
        let reference = closed_at.max(session.created_at.saturating_add(CREATION_SLACK_MS));
        match checkout_created_ms(worktree) {
            Some(created) if created > reference => return Some("created_after_close"),
            Some(_) => {}
            None => return Some("creation_time_unknown"),
        }
        // The one definition of a start's reservation, shared with start and
        // every removal primitive.
        crate::start_reservation::path_is_reserved(self.main_root(), worktree)
            .then_some("start_reserved")
    }

    /// Strip catalogued regenerable directories from closed checkouts, with
    /// the evidence of `run` (open sessions' paths resolved once, one
    /// open-file observation), at most `cap` checkouts, until `deadline`.
    /// Starts from the folders that exist (one listing per pool directory),
    /// each judged once through the row that owns it ([`judges_before`]),
    /// oldest close first; the backlog is counted only afterwards, within
    /// its own small budget. Never waits for a lock: a checkout whose
    /// lifecycle claim is busy is left for the next pass. Every outcome is
    /// counted by reason in the command's phase counters.
    pub(crate) fn strip_closed_checkouts(
        &self,
        run: &mut super::retire::RetireRun,
        deadline: Instant,
        cap: usize,
    ) -> CheckoutStripReport {
        let _span = crate::phase_telemetry::span("checkout_strip_pass");
        let mut report = CheckoutStripReport::default();
        let deadline = deadline.min(run.deadline());
        let rows = match self
            .store_ref()
            .closed_spawned_sessions_oldest_closed_first(STRIP_CANDIDATES_MAX)
        {
            Ok(sessions) => sessions,
            Err(error) => {
                crate::warn_unrecorded::<(), _>("list closed checkouts to strip", Err(error));
                return report;
            }
        };
        let live = self.store_ref().live_sessions().unwrap_or_default();
        let open_paths = live
            .iter()
            .filter(|session| session.cleanup_state == crate::SessionCleanupState::Open)
            .map(|session| session.worktree_path.clone())
            .collect::<BTreeSet<_>>();
        // One judge per recorded path, from the records alone (no
        // filesystem call), oldest close first, resumed from where the last
        // pass stopped.
        let mut by_path: BTreeMap<&str, &Session> = BTreeMap::new();
        for row in &rows {
            let path = row.worktree_path.as_str();
            if open_paths.contains(path) {
                continue;
            }
            match by_path.get_mut(path) {
                Some(current) => {
                    if judges_before(row, current) {
                        *current = row;
                    }
                }
                None => {
                    by_path.insert(path, row);
                }
            }
        }
        let mut judges = by_path.into_values().collect::<Vec<_>>();
        judges.sort_by_key(|session| judge_key(session));
        let cursor: Option<(i64, i64)> = self.meta_json(CHECKOUT_STRIP_CURSOR_KEY);
        let start = cursor.map_or(0, |cursor| {
            judges.partition_point(|session| judge_key(session) <= cursor)
        });
        judges.rotate_left(start);
        let mut listing = PoolListing::default();
        let listing_deadline = (Instant::now() + POOL_LISTING_BUDGET).min(deadline);
        let mut next_cursor = cursor;
        let mut lap_complete = true;
        let done: BTreeMap<i64, i64> = self.meta_json(CHECKOUT_STRIP_DONE_KEY);
        let backoff: BTreeMap<i64, StripBackoff> = self.meta_json(CHECKOUT_STRIP_BACKOFF_KEY);
        let venv_markers = crate::auto_cleanup::venv_markers(self.main_root());
        let mut newly_done = BTreeMap::new();
        let absent: BTreeMap<i64, i64> = self.meta_json(CHECKOUT_STRIP_ABSENT_KEY);
        let mut newly_absent = BTreeMap::new();
        let mut absent_seen_again = BTreeSet::new();
        let mut backoff_updates: BTreeMap<i64, Option<StripBackoff>> = BTreeMap::new();
        let mut holders = crate::session_holder::RecordedHolderProcessTable::default();
        for session in judges {
            let closed_at = session.closed_at.unwrap_or(0);
            if report.checkouts_listed >= cap || Instant::now() >= deadline {
                report.more_remaining = true;
                lap_complete = false;
                crate::phase_telemetry::add(
                    if report.checkouts_listed >= cap {
                        "checkout_strip_stopped_cap"
                    } else {
                        "checkout_strip_stopped_deadline"
                    },
                    Duration::ZERO,
                );
                break;
            }
            next_cursor = Some(judge_key(session));
            if done.get(&session.id) == Some(&closed_at) {
                report.skipped_remembered += 1;
                crate::phase_telemetry::add("checkout_strip_skipped_remembered", Duration::ZERO);
                continue;
            }
            // Backed off after transient keeps: not looked at, and not
            // counted toward the cap, so it never starves newer checkouts.
            let previous = backoff
                .get(&session.id)
                .filter(|entry| entry.closed_at == closed_at);
            if previous.is_some_and(|entry| entry.until_ms > crate::clock::epoch_ms()) {
                report.skipped_backoff += 1;
                crate::phase_telemetry::add("checkout_strip_skipped_backoff", Duration::ZERO);
                continue;
            }
            // The folder's existence, just before judging: from its pool
            // directory's listing (one listing per pool, entry types only),
            // else checked by `strip_one_checkout` itself.
            let listing_started = Instant::now();
            let exists = listing.exists(&session.worktree_path, listing_deadline);
            report.folder_listing_ms = report
                .folder_listing_ms
                .saturating_add(elapsed_ms(listing_started));
            if exists == Some(false) {
                report.folders_missing += 1;
                continue;
            }
            // Proven absent by an earlier pass: skipped while the listing
            // cannot say otherwise; seen again in its pool's listing, it is
            // judged again and the memory dropped.
            if absent.get(&session.id) == Some(&closed_at) {
                if exists == Some(true) {
                    absent_seen_again.insert(session.id);
                    crate::phase_telemetry::add("checkout_strip_absent_seen_again", Duration::ZERO);
                } else {
                    report.folders_missing += 1;
                    continue;
                }
            }
            report.folders_found += 1;
            report.checkouts_examined += 1;
            crate::phase_telemetry::add("checkout_strip_examined", Duration::ZERO);
            let transient_before = transient_refusals(&report);
            let checkout_deadline = deadline.min(Instant::now() + STRIP_CHECKOUT_BUDGET);
            match self.strip_one_checkout(
                session,
                run,
                &mut holders,
                checkout_deadline,
                StripContext {
                    venv_markers: &venv_markers,
                    finishing_caller: None,
                },
                &mut report,
            ) {
                CheckoutStrip::Done(directories) => {
                    if directories > 0 {
                        report.checkouts_stripped += 1;
                        report.directories_stripped += directories;
                        crate::phase_telemetry::add(
                            "checkout_strip_stripped_checkout",
                            Duration::ZERO,
                        );
                    }
                    // A directory kept only because a probe failed, time ran
                    // out or a rename failed is asked again, after a backoff
                    // that doubles per failure; a clean outcome resets it.
                    if transient_refusals(&report) == transient_before {
                        newly_done.insert(session.id, closed_at);
                        if backoff.contains_key(&session.id) {
                            backoff_updates.insert(session.id, None);
                            crate::phase_telemetry::add(
                                "checkout_strip_backoff_cleared",
                                Duration::ZERO,
                            );
                        }
                    } else {
                        let failures = previous.map_or(0, |entry| entry.failures) + 1;
                        backoff_updates.insert(
                            session.id,
                            Some(StripBackoff {
                                closed_at,
                                failures,
                                until_ms: crate::clock::epoch_ms()
                                    .saturating_add(strip_backoff_delay_ms(failures)),
                            }),
                        );
                        report.backoff_set += 1;
                        crate::phase_telemetry::add("checkout_strip_backoff_set", Duration::ZERO);
                    }
                }
                CheckoutStrip::Refused(code) => {
                    note_refusal(&mut report, code);
                    // Ownership never changes for a session: not asked again.
                    if code == "adopted" || code == "not_owned" || code == "tombstone" {
                        newly_done.insert(session.id, closed_at);
                    }
                    // A folder proven absent (ENOENT) while its pool
                    // directory exists and lists (so an unmounted or
                    // unreachable volume is never remembered as absent) is
                    // not stat-ed again for this close, so the time goes to
                    // folders that exist (0.8.123: 688 of 900 examined were
                    // absent).
                    if code == "checkout_missing"
                        && Path::new(&session.worktree_path)
                            .parent()
                            .is_some_and(|pool| std::fs::read_dir(pool).is_ok())
                    {
                        newly_absent.insert(session.id, closed_at);
                    }
                    if code == "process_use_unknown" {
                        report.process_use_unavailable = true;
                        report.more_remaining = true;
                        break;
                    }
                    if code == "deadline" && Instant::now() >= deadline {
                        report.more_remaining = true;
                        lap_complete = false;
                        break;
                    }
                }
            }
        }
        if !newly_done.is_empty() {
            crate::warn_unrecorded(
                "remember stripped checkouts",
                self.update_meta_json(
                    CHECKOUT_STRIP_DONE_KEY,
                    |stored: &mut BTreeMap<i64, i64>| {
                        stored.extend(newly_done.iter().map(|(id, at)| (*id, *at)));
                        while stored.len() > STRIP_DONE_MAX {
                            stored.pop_first();
                        }
                    },
                ),
            );
        }
        if !newly_absent.is_empty() || !absent_seen_again.is_empty() {
            crate::warn_unrecorded(
                "remember absent checkout folders",
                self.update_meta_json(
                    CHECKOUT_STRIP_ABSENT_KEY,
                    |stored: &mut BTreeMap<i64, i64>| {
                        for id in &absent_seen_again {
                            stored.remove(id);
                        }
                        stored.extend(newly_absent.iter().map(|(id, at)| (*id, *at)));
                        while stored.len() > STRIP_DONE_MAX {
                            stored.pop_first();
                        }
                    },
                ),
            );
        }
        if report.folders_missing > 0 {
            crate::phase_telemetry::add("checkout_strip_folders_missing", Duration::ZERO);
        }
        // A lap that reached its end starts from the oldest close again.
        let next_cursor = if lap_complete { None } else { next_cursor };
        if next_cursor != cursor {
            crate::warn_unrecorded(
                "remember where the strip pass stopped",
                self.update_meta_json(
                    CHECKOUT_STRIP_CURSOR_KEY,
                    |stored: &mut Option<(i64, i64)>| {
                        *stored = next_cursor;
                    },
                ),
            );
        }
        if !backoff_updates.is_empty() {
            crate::warn_unrecorded(
                "remember strip retry backoffs",
                self.update_meta_json(
                    CHECKOUT_STRIP_BACKOFF_KEY,
                    |stored: &mut BTreeMap<i64, StripBackoff>| {
                        for (id, update) in &backoff_updates {
                            match update {
                                Some(entry) => {
                                    stored.insert(*id, *entry);
                                }
                                None => {
                                    stored.remove(id);
                                }
                            }
                        }
                        while stored.len() > STRIP_BACKOFF_ENTRIES_MAX {
                            let Some((&earliest, _)) =
                                stored.iter().min_by_key(|(_, entry)| entry.until_ms)
                            else {
                                break;
                            };
                            stored.remove(&earliest);
                        }
                    },
                ),
            );
        }
        if report.directories_stripped > 0 {
            crate::phase_telemetry::add("checkout_strip_checkouts_stripped", Duration::ZERO);
        }
        // Counted after the strip, so counting never spends its budget, and
        // the per-folder part only once enough checkouts were judged.
        let backlog_deadline = Instant::now() + BACKLOG_BUDGET;
        let count_regenerable =
            report.checkouts_examined >= STRIP_MIN_JUDGED_BEFORE_ACCOUNTING || lap_complete;
        report.backlog = self.observe_checkout_backlog(
            &mut listing,
            &live,
            &rows,
            &open_paths,
            backlog_deadline,
            count_regenerable,
        );
        crate::phase_telemetry::attach(
            "checkout_strip",
            serde_json::json!({
                "folders_found": report.folders_found,
                "skipped_remembered": report.skipped_remembered,
                "folders_missing": report.folders_missing,
                "lap_complete": lap_complete,
                "skipped_backoff": report.skipped_backoff,
                "backoff_set": report.backoff_set,
                "venv_unexpected": report.venv_unexpected,
                "checkouts_examined": report.checkouts_examined,
                "checkouts_listed": report.checkouts_listed,
                "candidates_found": report.candidates_found,
                "checkouts_stripped": report.checkouts_stripped,
                "directories_stripped": report.directories_stripped,
                "refused": report.refused,
                "more_remaining": report.more_remaining,
                "phase_ms": {
                    "folder_listing": report.folder_listing_ms,
                    "discover": report.discover_ms,
                    "prove": report.prove_ms,
                    "move": report.move_ms,
                },
                "backlog": report.backlog,
            }),
        );
        report
    }

    /// Strip the closed session `session_id`'s checkout at `finish`, under
    /// the same rules as the lane (a fresh open-file observation; a process
    /// still in the checkout, such as the finishing shell, keeps it for the
    /// lane). Returns the directories moved into the trash.
    pub(crate) fn strip_checkout_at_finish(&self, session_id: i64) -> Result<usize, String> {
        // The finishing agent, so its own holder record does not keep the
        // checkout it is leaving (0.8.122 refused 3 of 3 at finish as
        // `holder_alive`: the holder was the agent running `finish`).
        let caller = match crate::session_holder::caller() {
            crate::session_holder::Caller::Agent(agent) => Some(agent),
            crate::session_holder::Caller::Unidentified => None,
        };
        self.strip_checkout_at_finish_as(session_id, caller.as_ref())
    }

    /// [`Self::strip_checkout_at_finish`] for the finishing `caller`.
    pub(crate) fn strip_checkout_at_finish_as(
        &self,
        session_id: i64,
        caller: Option<&crate::session_holder::AgentProcess>,
    ) -> Result<usize, String> {
        let _span = crate::phase_telemetry::span("checkout_strip_at_finish");
        let session = self
            .store_ref()
            .session(session_id)
            .map_err(|error| error.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let Some(mut run) = self
            .begin_checkout_retirement_run(deadline)
            .map_err(|error| error.to_string())?
        else {
            return Err("the open sessions' checkouts could not be resolved in time".into());
        };
        let mut report = CheckoutStripReport::default();
        let mut holders = crate::session_holder::RecordedHolderProcessTable::default();
        let venv_markers = crate::auto_cleanup::venv_markers(self.main_root());
        match self.strip_one_checkout(
            &session,
            &mut run,
            &mut holders,
            deadline,
            StripContext {
                venv_markers: &venv_markers,
                finishing_caller: caller,
            },
            &mut report,
        ) {
            CheckoutStrip::Done(directories) => {
                if directories > 0 {
                    crate::phase_telemetry::add(
                        "checkout_strip_at_finish_stripped",
                        Duration::ZERO,
                    );
                }
                Ok(directories)
            }
            CheckoutStrip::Refused(code) => {
                crate::phase_telemetry::add(refused_counter(code), Duration::ZERO);
                Err(format!("kept for the retirement lane ({code})"))
            }
        }
    }

    /// Under storage pressure at `broker start`: strip closed checkouts
    /// (each checkout's renames under its own short lifecycle claim; the
    /// caller holds none) and then let the deleter run unpaced outside any
    /// claim, all within `budget`.
    pub(crate) fn strip_closed_checkouts_under_pressure(&self, budget: Duration) -> PressureRelief {
        let _span = crate::phase_telemetry::span("checkout_strip_under_pressure");
        let started = Instant::now();
        let deadline = started + budget;
        let strip_deadline = started + budget / 3;
        let report = {
            let _inventory = crate::git::WorktreeInventoryRunScope::enter();
            match self.begin_checkout_retirement_run(strip_deadline) {
                Ok(Some(mut run)) => {
                    self.strip_closed_checkouts(&mut run, strip_deadline, STRIP_CHECKOUTS_PER_RUN)
                }
                Ok(None) | Err(_) => CheckoutStripReport::default(),
            }
        };
        let strip_ms = elapsed_ms(started);
        let drain_started = Instant::now();
        let drained = self.drain_checkout_trash(deadline);
        PressureRelief {
            report,
            freed: drained.bytes,
            strip_ms,
            drain_ms: elapsed_ms(drain_started),
        }
    }

    fn strip_one_checkout(
        &self,
        session: &Session,
        run: &mut super::retire::RetireRun,
        holders: &mut crate::session_holder::RecordedHolderProcessTable,
        deadline: Instant,
        context: StripContext<'_>,
        report: &mut CheckoutStripReport,
    ) -> CheckoutStrip {
        let StripContext {
            venv_markers,
            finishing_caller,
        } = context;
        use CheckoutStrip::Refused;
        if Instant::now() >= deadline {
            return Refused("deadline");
        }
        let _git_deadline = crate::git::limit_git_until(deadline);
        let preconditions = crate::phase_telemetry::span("checkout_strip_preconditions");
        if !session.status.is_closed()
            || !matches!(
                session.cleanup_state,
                crate::SessionCleanupState::Closed | crate::SessionCleanupState::Cleaned
            )
        {
            return Refused("not_closed");
        }
        if session.origin != crate::SessionOrigin::Spawned {
            return Refused("adopted");
        }
        let worktree = PathBuf::from(&session.worktree_path);
        // A retired checkout's tombstone holds nothing to strip.
        if super::tombstone::is_checkout_tombstone(&worktree) {
            return Refused("tombstone");
        }
        // Absent only when the filesystem says so; a call that fails any
        // other way is not proof the folder is gone.
        match std::fs::symlink_metadata(&worktree) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Refused("not_a_real_directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Refused("checkout_missing");
            }
            Err(_) => return Refused("checkout_unreadable"),
        }
        if let Some(code) = run.open_session_keep(session.id, &worktree) {
            return Refused(code);
        }
        match self
            .store_ref()
            .other_open_session_at_path(session.id, &session.worktree_path)
        {
            Ok(false) => {}
            Ok(true) => return Refused("open_session"),
            Err(_) => return Refused("open_session_unknown"),
        }
        let Some(pool) = self.trash_pool_for(session, &worktree) else {
            return Refused("not_owned");
        };
        match self
            .store_ref()
            .unreleased_unexpired_leases_for_sessions_at(&[session.id], crate::clock::epoch_ms())
        {
            Ok(leases) if leases.is_empty() => {}
            Ok(_) => return Refused("lease_held"),
            Err(_) => return Refused("lease_unknown"),
        }
        match crate::gates::checked_running_session_gates_until(
            self.main_root(),
            session.id,
            || Instant::now() >= deadline,
            256,
        ) {
            Some(gates) if gates.is_empty() => {}
            Some(_) => return Refused("gate_running"),
            None => return Refused("gates_unknown"),
        }
        match self.session_operations_unresolved_since_boot(session.id) {
            Ok(false) => {}
            Ok(true) => return Refused("operation_unresolved"),
            Err(_) => return Refused("operations_unknown"),
        }
        // At `finish`, the recorded holder is usually the agent finishing
        // the session: it is leaving, so it does not keep the checkout (a
        // shell of it still in the checkout does, by the open-file check).
        let holder_is_finishing_caller = finishing_caller.is_some_and(|caller| {
            crate::session_holder::recorded_holder_is(self.store_ref(), session.id, caller)
        });
        if holder_is_finishing_caller {
            crate::phase_telemetry::add(
                "checkout_strip_holder_is_finishing_caller",
                Duration::ZERO,
            );
        } else {
            if let Some(code) = self.closed_checkout_holder_keep(session, holders) {
                return Refused(code);
            }
        }
        drop(preconditions);
        report.checkouts_listed += 1;
        let discover_started = Instant::now();
        let candidates = regenerable_candidates(&worktree, deadline);
        report.discover_ms = report
            .discover_ms
            .saturating_add(elapsed_ms(discover_started));
        let candidates = match candidates {
            Ok(candidates) => candidates,
            Err(_) => return Refused("discovery_failed"),
        };
        report.candidates_found += candidates.len();
        for candidate in &candidates {
            crate::phase_telemetry::add(candidate.kind.found_counter(), Duration::ZERO);
        }
        if candidates.is_empty() {
            crate::phase_telemetry::add("checkout_strip_nothing_found", Duration::ZERO);
            return CheckoutStrip::Done(0);
        }
        // Only a checkout with something to strip pays for the open-file
        // observation (the lane's single one is shared by the whole run).
        let snapshot = match run.process_use() {
            Ok(snapshot) => snapshot,
            Err(_) => return Refused("process_use_unknown"),
        };
        if crate::reclaim::open_file_snapshot_blocker_for_worktree(&snapshot, &worktree).is_some() {
            return Refused("process_using_checkout");
        }
        // The renames, and the cheap proofs right before them, under the
        // lifecycle claim (taken without waiting, so a foreground command
        // never queues behind this pass).
        let _claim = match self.try_claim_worktree_lifecycle("retire") {
            Ok(Some(claim)) => claim,
            Ok(None) | Err(_) => return Refused("lifecycle_busy"),
        };
        let current = match self.store_ref().session(session.id) {
            Ok(current)
                if current.status.is_closed()
                    && current.cleanup_state == session.cleanup_state
                    && current.worktree_path == session.worktree_path =>
            {
                current
            }
            _ => return Refused("changed"),
        };
        if let Some(code) = self.reused_path_keep(&current, &worktree, !holder_is_finishing_caller)
        {
            return Refused(code);
        }
        let prove_started = Instant::now();
        let verdicts = prove_strippable_all(&worktree, &candidates, deadline, venv_markers);
        report.prove_ms = report.prove_ms.saturating_add(elapsed_ms(prove_started));
        let mut stripped = 0_usize;
        for (candidate, verdict) in candidates.iter().zip(verdicts) {
            if let Err(code) = verdict {
                note_refusal(report, code);
                // Name what kept an environment, so its owner can declare a
                // marker their own tooling writes (names only).
                if code == "unexpected_content" && candidate.kind == StripKind::Venv {
                    for name in
                        venv_unexpected_names(&worktree.join(&candidate.relative), venv_markers)
                            .unwrap_or_default()
                    {
                        if report.venv_unexpected.len() >= VENV_UNEXPECTED_NAMES_MAX {
                            break;
                        }
                        report
                            .venv_unexpected
                            .insert(name.chars().take(REPORTED_NAME_MAX).collect());
                    }
                }
                continue;
            }
            if Instant::now() >= deadline {
                note_refusal(report, "deadline");
                break;
            }
            let _move_span = crate::phase_telemetry::span("checkout_strip_move");
            let move_started = Instant::now();
            let moved = crate::checkout_trash::move_directory_to_trash(
                &pool,
                &worktree,
                &candidate.relative,
                session.id,
                &mut |trash_dir, entry, row| self.record_checkout_trash_row(trash_dir, entry, row),
            );
            report.move_ms = report.move_ms.saturating_add(elapsed_ms(move_started));
            match moved {
                Ok(moved) => {
                    crate::warn_unrecorded(
                        "confirm a stripped directory's trash entry",
                        self.record_checkout_trash_row(&moved.trash_dir, &moved.entry, moved.row),
                    );
                    stripped += 1;
                    crate::phase_telemetry::add(
                        "checkout_strip_stripped_directory",
                        Duration::ZERO,
                    );
                    crate::phase_telemetry::add(candidate.kind.stripped_counter(), Duration::ZERO);
                }
                Err(crate::checkout_trash::StripMoveRefusal::NotARealDirectory(_)) => {
                    note_refusal(report, "not_a_real_directory");
                }
                Err(crate::checkout_trash::StripMoveRefusal::CrossesDevice) => {
                    note_refusal(report, "crosses_device");
                }
                Err(crate::checkout_trash::StripMoveRefusal::Failed(_)) => {
                    note_refusal(report, "move_failed");
                }
            }
        }
        CheckoutStrip::Done(stripped)
    }

    /// Count checkout folders from the broker's records and the pool
    /// listings (no call per recorded session, no path resolution), then,
    /// when `count_regenerable`, one `lstat` per top-level catalogued name
    /// of a closed folder until `deadline`; no byte walk. A folder counts as
    /// closed when no open session records its exact path. Folders of a
    /// pool that could not be listed are not counted (`regenerable_partial`).
    fn observe_checkout_backlog(
        &self,
        listing: &mut PoolListing,
        live: &[Session],
        closed: &[Session],
        open_paths: &BTreeSet<String>,
        deadline: Instant,
        count_regenerable: bool,
    ) -> CheckoutBacklog {
        let _span = crate::phase_telemetry::span("checkout_strip_backlog");
        #[cfg(test)]
        BEFORE_BACKLOG.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook();
            }
        });
        let mut backlog = CheckoutBacklog {
            observed_at_ms: crate::clock::epoch_ms(),
            ..CheckoutBacklog::default()
        };
        let mut folders = BTreeSet::new();
        for session in live.iter().chain(closed.iter()) {
            if folders.contains(session.worktree_path.as_str()) {
                continue;
            }
            match listing.exists(&session.worktree_path, deadline) {
                Some(true) => {
                    folders.insert(session.worktree_path.as_str());
                }
                Some(false) => {}
                None => backlog.regenerable_partial = true,
            }
        }
        backlog.folders = folders.len();
        let closed_folders = closed
            .iter()
            .map(|session| session.worktree_path.as_str())
            .filter(|path| folders.contains(path) && !open_paths.contains(*path))
            .collect::<BTreeSet<_>>();
        backlog.closed_folders = closed_folders.len();
        if !count_regenerable {
            backlog.regenerable_partial = true;
            crate::phase_telemetry::add("checkout_strip_backlog_deferred", Duration::ZERO);
            return backlog;
        }
        for path in closed_folders {
            if Instant::now() >= deadline {
                backlog.regenerable_partial = true;
                crate::phase_telemetry::add("checkout_strip_backlog_partial", Duration::ZERO);
                break;
            }
            if crate::artifact_catalog::CATALOG
                .iter()
                .filter(|kind| kind.unattended)
                .any(|kind| {
                    std::fs::symlink_metadata(Path::new(path).join(kind.name)).is_ok_and(
                        |metadata| metadata.is_dir() && !metadata.file_type().is_symlink(),
                    )
                })
            {
                backlog.closed_with_regenerable += 1;
            }
        }
        backlog
    }

    /// The strip progress after `report`'s run, from `previous`.
    pub(crate) fn next_checkout_strip_progress(
        &self,
        previous: &CheckoutStripProgress,
        report: &super::retire::CheckoutRetirementReport,
    ) -> CheckoutStripProgress {
        let strip = &report.strip;
        let now_ms = crate::clock::epoch_ms();
        let today = now_ms.div_euclid(DAY_MS);
        let mut days = previous.days.clone();
        let day = days.entry(today).or_default();
        if let Ok(closed) = self.store_ref().count_sessions_closed_since(today * DAY_MS) {
            day.sessions_closed = closed;
        }
        if let Ok(created) = self
            .store_ref()
            .count_sessions_created_since(today * DAY_MS)
        {
            day.sessions_created = created;
        }
        day.cleaned_leftovers_retired = day
            .cleaned_leftovers_retired
            .saturating_add(report.retired_cleaned_leftovers);
        day.checkouts_retired = day.checkouts_retired.saturating_add(
            report.retired_clean + report.retired_archived + report.retired_operation_archived,
        );
        day.checkouts_stripped = day
            .checkouts_stripped
            .saturating_add(strip.checkouts_stripped);
        day.checkouts_archived = day
            .checkouts_archived
            .saturating_add(report.retired_archived + report.retired_operation_archived);
        day.archive_bytes = day.archive_bytes.saturating_add(report.archive_bytes);
        day.directories_stripped = day
            .directories_stripped
            .saturating_add(strip.directories_stripped);
        day.bytes_freed = day.bytes_freed.saturating_add(report.trash.stripped_bytes);
        while days.len() > STRIP_DAYS_KEPT {
            days.pop_first();
        }
        let declared = crate::auto_cleanup::venv_markers(self.main_root());
        let mut venv_unexpected = previous.venv_unexpected.clone();
        for name in &strip.venv_unexpected {
            venv_unexpected.insert(name.clone(), now_ms);
        }
        venv_unexpected.retain(|name, seen| {
            now_ms.saturating_sub(*seen) < VENV_UNEXPECTED_KEPT_MS && !declared.contains(name)
        });
        while venv_unexpected.len() > VENV_UNEXPECTED_NAMES_MAX {
            let Some(oldest) = venv_unexpected
                .iter()
                .min_by_key(|(_, seen)| **seen)
                .map(|(name, _)| name.clone())
            else {
                break;
            };
            venv_unexpected.remove(&oldest);
        }
        let backlog = if strip.backlog.observed_at_ms > 0 {
            strip.backlog.clone()
        } else {
            previous.backlog.clone()
        };
        CheckoutStripProgress {
            last_checkouts_examined: strip.checkouts_examined,
            last_checkouts_stripped: strip.checkouts_stripped,
            last_directories_stripped: strip.directories_stripped,
            last_refused: strip.refused.clone(),
            last_phase_ms: BTreeMap::from([
                ("folder_listing".to_string(), strip.folder_listing_ms),
                ("discover".to_string(), strip.discover_ms),
                ("prove".to_string(), strip.prove_ms),
                ("move".to_string(), strip.move_ms),
            ]),
            backlog,
            bytes_freed_total: previous
                .bytes_freed_total
                .saturating_add(report.trash.stripped_bytes),
            days,
            venv_unexpected,
        }
    }

    /// The strip progress the last retirement run recorded.
    #[cfg(test)]
    pub(crate) fn checkout_strip_progress(&self) -> CheckoutStripProgress {
        self.checkout_retirement_progress().strip
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repository with an ignored top-level and nested `node_modules`, an
    /// unignored `vendor` and a tracked file.
    fn checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), "node_modules/\ntarget/\n").unwrap();
        std::fs::write(root.join("README.md"), "readme\n").unwrap();
        std::fs::create_dir_all(root.join("packages/app")).unwrap();
        std::fs::write(root.join("packages/app/package.json"), "{}\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "init"]);
        for dir in ["node_modules/left-pad", "packages/app/node_modules/dep"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("index.js"), "x\n").unwrap();
        }
        for dir in ["node_modules", "packages/app/node_modules"] {
            std::fs::write(root.join(dir).join(".package-lock.json"), "{}\n").unwrap();
        }
        dir
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    #[test]
    fn collapsed_listing_finds_top_level_and_nested_node_modules() {
        let dir = checkout();
        let root = dir.path().canonicalize().unwrap();
        let found = regenerable_candidates(&root, far()).unwrap();
        let mut names = found
            .iter()
            .map(|candidate| candidate.relative.clone())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["node_modules", "packages/app/node_modules"]);
        for candidate in &found {
            assert_eq!(prove_strippable(&root, candidate, far()), Ok(()));
        }
    }

    /// Git's collapsed listing prints an ignored directory once and does not
    /// list its contents, however many files it holds.
    #[test]
    fn collapsed_listing_does_not_list_inside_an_ignored_directory() {
        let dir = checkout();
        let root = dir.path().canonicalize().unwrap();
        for index in 0..2_000 {
            let sub = root.join(format!("node_modules/pkg{index}"));
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join("index.js"), "x\n").unwrap();
        }
        let output = Command::new("git")
            .args([
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
                "-z",
            ])
            .current_dir(&root)
            .output()
            .unwrap();
        let records = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .map(|record| String::from_utf8_lossy(record).into_owned())
            .collect::<Vec<_>>();
        assert!(
            records.contains(&"node_modules/".to_string()),
            "{records:?}"
        );
        assert!(
            records
                .iter()
                .all(|record| !record.starts_with("node_modules/pkg")),
            "{records:?}"
        );
    }

    #[test]
    fn a_tracked_file_inside_refuses() {
        let dir = checkout();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("node_modules/keep.txt"), "tracked\n").unwrap();
        git(&root, &["add", "-f", "node_modules/keep.txt"]);
        let candidate = StripCandidate {
            relative: "node_modules".into(),
            kind: StripKind::NodeModules,
        };
        // `check-ignore` already reports a directory holding a tracked
        // file as not ignored; the index read refuses it independently.
        assert!(
            matches!(
                prove_strippable(&root, &candidate, far()),
                Err("holds_tracked" | "not_ignored")
            ),
            "a directory holding a tracked file must be refused"
        );
    }

    #[test]
    fn an_unignored_directory_refuses() {
        let dir = checkout();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        let candidate = StripCandidate {
            relative: "node_modules".into(),
            kind: StripKind::NodeModules,
        };
        assert_eq!(
            prove_strippable(&root, &candidate, far()),
            Err("not_ignored")
        );
        // Discovery does not ask Git: it lists the name, and the proof
        // keeps it.
        let found = regenerable_candidates(&root, far()).unwrap();
        let verdicts = prove_strippable_all(&root, &found, far(), &[]);
        for (candidate, verdict) in found.iter().zip(&verdicts) {
            if candidate.relative.ends_with("node_modules") {
                assert_eq!(*verdict, Err("not_ignored"), "{candidate:?}");
            }
        }
    }

    /// Discovery runs no Git (a checkout whose Git directory is gone is
    /// still searched) and finds catalogued directories at the top level,
    /// in a sub-project and in a workspace member, without entering a
    /// catalogued directory, `.aethyme`, a symlink or anything deeper.
    #[test]
    fn discovery_reads_no_git_and_finds_nested_sub_projects() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join(".git"), "gitdir: /nonexistent/worktrees/x\n").unwrap();
        for path in [
            "node_modules/dep/node_modules/inner",
            "backend/.venv/lib",
            "backend/src",
            "apps/web/node_modules/dep",
            "apps/web/src",
            "a/b/c/node_modules/too-deep",
            ".aethyme/node_modules/x",
            "services/target/debug",
            "services/api/target/debug",
        ] {
            std::fs::create_dir_all(root.join(path)).unwrap();
        }
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(elsewhere.path().join("node_modules/dep")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), root.join("linked")).unwrap();
        std::fs::write(root.join("README.md"), "x\n").unwrap();

        let found = regenerable_candidates(&root, far()).unwrap();

        let names = found
            .iter()
            .map(|candidate| (candidate.relative.as_str(), candidate.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                ("apps/web/node_modules", StripKind::NodeModules),
                ("backend/.venv", StripKind::Venv),
                ("node_modules", StripKind::NodeModules),
                ("services/target", StripKind::CargoTarget),
            ]
        );
        // Without a `.git` entry the checkout is not its own Git top level.
        std::fs::remove_file(root.join(".git")).unwrap();
        assert!(regenerable_candidates(&root, far()).is_err());
    }

    /// Two Git calls judge every candidate of a checkout, each verdict its
    /// own: ignored and untracked passes, unignored and tracked are kept.
    #[test]
    fn one_proof_judges_every_candidate_of_a_checkout() {
        let dir = checkout();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(
            root.join(".gitignore"),
            "node_modules/\ntarget/\n!tools/node_modules/\n",
        )
        .unwrap();
        for path in ["tools/node_modules", "lib/node_modules"] {
            std::fs::create_dir_all(root.join(path)).unwrap();
            std::fs::write(root.join(path).join(".package-lock.json"), "{}\n").unwrap();
        }
        std::fs::write(root.join("lib/node_modules/keep.js"), "tracked\n").unwrap();
        git(&root, &["add", "-f", "lib/node_modules/keep.js"]);
        let candidates = [
            "lib/node_modules",
            "node_modules",
            "packages/app/node_modules",
            "tools/node_modules",
        ]
        .map(|relative| StripCandidate {
            relative: relative.into(),
            kind: StripKind::NodeModules,
        });

        let verdicts = prove_strippable_all(&root, &candidates, far(), &[]);

        assert!(
            matches!(verdicts[0], Err("holds_tracked" | "not_ignored")),
            "{verdicts:?}"
        );
        assert_eq!(verdicts[1..], [Ok(()), Ok(()), Err("not_ignored")]);
    }

    /// Every row at a path closed or cleaned leaves exactly one judge: the
    /// newest closed row, else (all cleaned) any of them.
    #[test]
    fn exactly_one_row_judges_a_reused_path() {
        let row = |id: i64, cleanup_state: crate::SessionCleanupState| Session {
            id,
            worktree_path: "/pool/reused".into(),
            branch: format!("agent/reused-{id}"),
            origin: crate::SessionOrigin::Spawned,
            status: crate::SessionStatus::Cleaned,
            cleanup_state,
            closed_at: Some(id * 1_000),
            cleanup_completed_at: None,
            task: None,
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            accepted_session_head: None,
            accepted_integration_commit: None,
            accepted_integration_tree: None,
            accepted_queue_entry_id: None,
            accepted_at: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            exit_code: None,
            agent_identity: None,
            repository_name: None,
            tab_name: None,
            ai_provider: None,
            short_name: None,
            created_at: id * 1_000 - 500,
            updated_at: id * 1_000,
            last_activity_at: id * 1_000,
        };
        use crate::SessionCleanupState::{Cleaned, Closed};
        let all_cleaned = [row(1, Cleaned), row(2, Cleaned), row(3, Cleaned)];
        for session in &all_cleaned {
            assert!(!path_superseded_by(session, &all_cleaned), "{}", session.id);
        }
        let mixed = [
            row(1, Closed),
            row(2, Cleaned),
            row(3, Closed),
            row(4, Cleaned),
        ];
        let judges = mixed
            .iter()
            .filter(|session| !path_superseded_by(session, &mixed))
            .map(|session| session.id)
            .collect::<Vec<_>>();
        assert_eq!(judges, [3]);
        let best = mixed
            .iter()
            .reduce(|best, row| if judges_before(row, best) { row } else { best })
            .unwrap();
        assert_eq!(best.id, 3);
    }

    #[test]
    fn a_symlinked_directory_refuses() {
        let dir = checkout();
        let root = dir.path().canonicalize().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("index.js"), "x\n").unwrap();
        std::fs::remove_dir_all(root.join("node_modules")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), root.join("node_modules")).unwrap();
        let candidate = StripCandidate {
            relative: "node_modules".into(),
            kind: StripKind::NodeModules,
        };
        assert_eq!(prove_strippable(&root, &candidate, far()), Err("symlink"));
        // A symlinked parent refuses the nested directory too.
        let real_packages = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(real_packages.path().join("node_modules/dep")).unwrap();
        std::fs::write(real_packages.path().join("node_modules/dep/a.js"), "x\n").unwrap();
        std::os::unix::fs::symlink(real_packages.path(), root.join("linked")).unwrap();
        let nested = StripCandidate {
            relative: "linked/node_modules".into(),
            kind: StripKind::NodeModules,
        };
        assert_eq!(prove_strippable(&root, &nested, far()), Err("symlink"));
        assert!(elsewhere.path().join("index.js").exists());
    }

    /// Every witness fails closed: a directory with the right name but not
    /// the producing tool's marker or layout is kept, with its reason.
    #[test]
    fn witnesses_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let at = |name: &str| {
            let path = dir.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        };
        let modules = at("a/node_modules");
        std::fs::write(modules.join("index.js"), "x\n").unwrap();
        assert_eq!(
            StripKind::NodeModules.witness(&modules, &[]),
            Err("witness_missing")
        );
        std::fs::write(modules.join(".yarn-integrity"), "{}\n").unwrap();
        assert_eq!(StripKind::NodeModules.witness(&modules, &[]), Ok(()));
        let pnpm = at("b/node_modules");
        std::fs::create_dir_all(pnpm.join(".pnpm")).unwrap();
        assert_eq!(StripKind::NodeModules.witness(&pnpm, &[]), Ok(()));
        // A nested repository or submodule is never regenerable.
        std::fs::write(pnpm.join(".git"), "gitdir: elsewhere\n").unwrap();
        assert_eq!(
            StripKind::NodeModules.witness(&pnpm, &[]),
            Err("nested_repository")
        );

        let store = at(".pnpm-store");
        std::fs::create_dir_all(store.join("files")).unwrap();
        assert_eq!(
            StripKind::PnpmStore.witness(&store, &[]),
            Err("witness_missing")
        );
        std::fs::create_dir_all(store.join("v10")).unwrap();
        assert_eq!(StripKind::PnpmStore.witness(&store, &[]), Ok(()));

        let venv = at(".venv");
        std::fs::create_dir_all(venv.join("lib")).unwrap();
        assert_eq!(StripKind::Venv.witness(&venv, &[]), Err("witness_missing"));
        std::fs::write(venv.join("pyvenv.cfg"), "home = /usr\n").unwrap();
        assert_eq!(StripKind::Venv.witness(&venv, &[]), Ok(()));
        // What environment tools write is part of it.
        std::fs::write(venv.join("CACHEDIR.TAG"), "Signature: x\n").unwrap();
        std::fs::write(venv.join(".lock"), "").unwrap();
        assert_eq!(StripKind::Venv.witness(&venv, &[]), Ok(()));
        // Any other dotfile keeps it, unless the repository declared that
        // exact name; `.env` keeps it whatever else is declared.
        std::fs::write(venv.join(".env-check.json"), "{}\n").unwrap();
        assert_eq!(
            StripKind::Venv.witness(&venv, &[]),
            Err("unexpected_content")
        );
        let declared = vec![".env-check.json".to_string(), ".other".to_string()];
        assert_eq!(StripKind::Venv.witness(&venv, &declared), Ok(()));
        std::fs::write(venv.join(".env"), "TOKEN=secret\n").unwrap();
        assert_eq!(
            StripKind::Venv.witness(&venv, &declared),
            Err("unexpected_content")
        );
        assert_eq!(
            venv_unexpected_names(&venv, &declared),
            Ok(vec![".env".to_string()])
        );
        std::fs::remove_file(venv.join(".env")).unwrap();
        // A declared name that is a directory, a hidden directory, or a
        // visible file someone put there keeps it.
        std::fs::create_dir_all(venv.join(".other")).unwrap();
        assert_eq!(
            StripKind::Venv.witness(&venv, &declared),
            Err("unexpected_content")
        );
        std::fs::remove_dir_all(venv.join(".other")).unwrap();
        std::fs::create_dir_all(venv.join(".cache")).unwrap();
        assert_eq!(
            StripKind::Venv.witness(&venv, &declared),
            Err("unexpected_content")
        );
        std::fs::remove_dir_all(venv.join(".cache")).unwrap();
        std::fs::write(venv.join("notes.txt"), "mine\n").unwrap();
        assert_eq!(
            StripKind::Venv.witness(&venv, &declared),
            Err("unexpected_content")
        );

        let target = at("target");
        std::fs::write(target.join("CACHEDIR.TAG"), "not a cache tag\n").unwrap();
        assert_eq!(
            StripKind::CargoTarget.witness(&target, &[]),
            Err("witness_missing")
        );
        std::fs::write(
            target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n# cargo\n",
        )
        .unwrap();
        assert_eq!(StripKind::CargoTarget.witness(&target, &[]), Ok(()));
        assert_eq!(StripKind::for_name("build"), None);
        assert_eq!(strip_backoff_delay_ms(1), 5 * 60_000);
        assert_eq!(strip_backoff_delay_ms(2), 10 * 60_000);
        assert_eq!(strip_backoff_delay_ms(40), 4 * 3_600_000);
        assert_eq!(StripKind::for_name("dist"), None);
    }

    /// A pnpm workspace member's `node_modules` (links into the top-level
    /// store, `.bin` shims, `@scope` directories of links) passes as that
    /// tool's output; the checkout's own top-level one never does without
    /// its marker, and anything else inside keeps it.
    #[test]
    fn a_pnpm_workspace_members_node_modules_has_a_narrow_witness() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let member = root.join("packages/ui/node_modules");
        std::fs::create_dir_all(member.join(".bin")).unwrap();
        std::fs::create_dir_all(member.join("@types")).unwrap();
        std::os::unix::fs::symlink(
            "../../../node_modules/.pnpm/react@19.2.8/node_modules/react",
            member.join("react"),
        )
        .unwrap();
        std::os::unix::fs::symlink("../../ui-kit", member.join("@types/ui-kit")).unwrap();
        std::fs::write(member.join(".bin/tsc"), "#!/bin/sh\nexec node tsc \"$@\"\n").unwrap();
        // pnpm writes the same shims into the top-level `.bin`.
        let top_bin = root.join("node_modules/.bin");
        std::fs::create_dir_all(&top_bin).unwrap();
        std::fs::write(top_bin.join("tsc"), "#!/bin/sh\n").unwrap();
        let candidate = |relative: &str| StripCandidate {
            relative: relative.into(),
            kind: StripKind::NodeModules,
        };
        assert!(pnpm_member_node_modules(&member, &top_bin));
        assert_eq!(
            prove_on_disk(&root, &candidate("packages/ui/node_modules"), &[]),
            Ok(())
        );

        // The checkout's top-level `node_modules` with the same content is
        // not a member: it needs its own marker.
        let top = root.join("node_modules");
        std::fs::create_dir_all(&top).unwrap();
        std::os::unix::fs::symlink(".pnpm/react@19.2.8/node_modules/react", top.join("react"))
            .unwrap();
        assert_eq!(
            prove_on_disk(&root, &candidate("node_modules"), &[]),
            Err("witness_missing")
        );

        // Links that never point into a pnpm store are not enough.
        let plain = root.join("tools/x/node_modules");
        std::fs::create_dir_all(&plain).unwrap();
        std::os::unix::fs::symlink("../../../elsewhere", plain.join("dep")).unwrap();
        assert!(!pnpm_member_node_modules(&plain, &top_bin));

        // A cache directory, a regular file, a file in a scope or a `.bin`
        // entry that is not a shim keeps the member.
        std::fs::create_dir_all(member.join(".vite")).unwrap();
        assert!(!pnpm_member_node_modules(&member, &top_bin));
        std::fs::remove_dir_all(member.join(".vite")).unwrap();
        std::fs::write(member.join("notes.txt"), "mine\n").unwrap();
        assert!(!pnpm_member_node_modules(&member, &top_bin));
        std::fs::remove_file(member.join("notes.txt")).unwrap();
        std::fs::write(member.join("@types/own.d.ts"), "x\n").unwrap();
        assert!(!pnpm_member_node_modules(&member, &top_bin));
        std::fs::remove_file(member.join("@types/own.d.ts")).unwrap();
        // A shim the top-level `.bin` does not also hold keeps it.
        std::fs::write(member.join(".bin/own-tool"), "#!/bin/sh\n").unwrap();
        assert!(!pnpm_member_node_modules(&member, &top_bin));
        std::fs::remove_file(member.join(".bin/own-tool")).unwrap();
        std::fs::write(member.join(".bin/data"), "not a shim\n").unwrap();
        assert!(!pnpm_member_node_modules(&member, &top_bin));
        std::fs::remove_file(member.join(".bin/data")).unwrap();
        assert!(pnpm_member_node_modules(&member, &top_bin));
    }
}
