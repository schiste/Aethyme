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
//!    `.pnpm-store`, `.venv`, Cargo `target`) among the
//!    collapsed ignored directories Git lists with
//!    `ls-files --others --ignored --exclude-standard --directory`, which
//!    prints an ignored directory as one `dir/` record and never descends
//!    into it (nested workspaces such as `packages/*/node_modules` are found
//!    because their parents are not ignored);
//! 2. prove each one cheaply and without recursion: `git check-ignore -q`
//!    on the directory itself (ignored), `git ls-files -- <dir>` printing
//!    nothing (no tracked file: an index read, bounded output), every path
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
//!   open or newer session at the path, a live holder, a folder born after
//!   the close, a start holding the folder's reservation);
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
/// Remembered handled sessions at most; the lowest ids go first.
const STRIP_DONE_MAX: usize = 16_384;
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

/// What one strip pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CheckoutStripReport {
    pub checkouts_examined: usize,
    /// Of those, checkouts that passed every precondition and were listed
    /// through Git; the per-run cap counts these, so checkouts refused
    /// before any Git work never starve newer ones.
    pub checkouts_listed: usize,
    pub checkouts_stripped: usize,
    pub directories_stripped: usize,
    /// Refusals by reason code: a checkout-level code keeps the whole
    /// checkout, a directory-level one only that directory.
    pub refused: BTreeMap<String, usize>,
    /// The cap or the deadline stopped the pass with checkouts left.
    pub more_remaining: bool,
    /// The run's open-file observation could not be taken: nothing was
    /// stripped from the checkout that needed it, and the pass stopped.
    pub process_use_unavailable: bool,
    /// Milliseconds spent listing, proving and renaming.
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

    /// Whether `path` (a real directory) is this tool's output. `Err` is
    /// the keep reason: `witness_missing`, `unexpected_content`, or
    /// `nested_repository` when a `.git` entry sits at its top (a nested
    /// repository or submodule is never regenerable). Reads only the
    /// directory's own top level (bounded) and at most one small file.
    pub(crate) fn witness(self, path: &Path) -> Result<(), &'static str> {
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
        let top_names = || -> Result<Vec<String>, &'static str> {
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
        };
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
                if top_names()?
                    .iter()
                    .all(|name| VENV_LAYOUT.contains(&name.as_str()))
                {
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

/// The first bytes of a `CACHEDIR.TAG` under the Cache Directory Tagging
/// specification.
const CACHEDIR_TAG_SIGNATURE: &[u8; 43] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// The catalogued regenerable directories of `worktree`, from Git's
/// collapsed ignored-directory listing (`ls-files --others --ignored
/// --exclude-standard --directory`): Git prints an ignored directory as one
/// record and does not read below it, so the listing never walks
/// `node_modules`. Only entries the unattended sweep may remove qualify, and
/// only once their witness is present; anything under `.aethyme` or a broker
/// runtime path is left alone.
pub(crate) fn regenerable_candidates(
    worktree: &Path,
    deadline: Instant,
) -> Result<Vec<StripCandidate>, String> {
    let _span = crate::phase_telemetry::span("checkout_strip_discover");
    let repo = GitRepo::discover(worktree).map_err(|error| error.to_string())?;
    let expected = worktree
        .canonicalize()
        .map_err(|error| format!("cannot resolve the checkout: {error}"))?;
    let discovered = repo
        .root()
        .canonicalize()
        .map_err(|error| format!("cannot resolve the discovered root: {error}"))?;
    if discovered != expected {
        return Err("Git resolved a different root than the checkout".into());
    }
    let entries = repo
        .ignored_paths_readonly_until(deadline)
        .map_err(|error| error.to_string())?;
    let mut candidates = Vec::new();
    for entry in entries {
        if !entry.directory_marker || entry.kind != crate::git::GitIgnoredPathKind::DirectoryRoot {
            continue;
        }
        let relative = entry.relative;
        if relative
            .split('/')
            .any(|part| part == ".aethyme" || part == ".git")
            || crate::runtime_paths::is_broker_runtime_path(&relative)
        {
            continue;
        }
        let name = relative.rsplit('/').next().unwrap_or(relative.as_str());
        let Some(kind) = StripKind::for_name(name) else {
            continue;
        };
        // The witness is judged by `prove_strippable`, which records why a
        // directory without it is kept.
        candidates.push(StripCandidate { relative, kind });
    }
    Ok(candidates)
}

/// Run one read-only Git probe in `worktree` under `deadline`; returns its
/// exit code and bounded stdout. Anything but a completed run is an error.
fn git_probe(worktree: &Path, args: &[&str], deadline: Instant) -> Result<(i32, Vec<u8>), String> {
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

/// Prove `candidate` may be stripped, cheaply and without recursion: every
/// component a real directory (no symlink), the directory on the checkout's
/// device, its witness still present, Git calling the directory itself
/// ignored, and the index holding no file under it. `Err` is the refusal
/// code; any probe that fails refuses.
pub(crate) fn prove_strippable(
    worktree: &Path,
    candidate: &StripCandidate,
    deadline: Instant,
) -> Result<(), &'static str> {
    use std::os::unix::fs::MetadataExt;
    let _span = crate::phase_telemetry::span("checkout_strip_prove");
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
    candidate.kind.witness(&path)?;
    // The directory itself, not its contents: exit 0 is ignored, 1 is not.
    // `check-ignore` takes plain paths and refuses pathspec magic, so no
    // `--literal-pathspecs` here.
    match git_probe(
        worktree,
        &[
            "-c",
            "core.fsmonitor=false",
            "check-ignore",
            "-q",
            "--",
            candidate.relative.as_str(),
        ],
        deadline,
    ) {
        Ok((0, _)) => {}
        Ok((1, _)) => return Err("not_ignored"),
        Ok(_) | Err(_) => return Err("probe_failed"),
    }
    // An index read with a literal pathspec: any output is a tracked file
    // under the directory. Never reads the working tree.
    match git_probe(
        worktree,
        &[
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
            "ls-files",
            "-z",
            "--",
            candidate.relative.as_str(),
        ],
        deadline,
    ) {
        Ok((0, stdout)) if stdout.is_empty() => Ok(()),
        Ok((0, _)) => Err("holds_tracked"),
        Ok(_) | Err(_) => Err("probe_failed"),
    }
}

/// The phase counter of one strip refusal, by code.
fn refused_counter(code: &str) -> &'static str {
    match code {
        "not_closed" => "checkout_strip_refused_not_closed",
        "adopted" => "checkout_strip_refused_adopted",
        "checkout_missing" => "checkout_strip_refused_checkout_missing",
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

impl Broker {
    /// What keeps `session`'s checkout `worktree`, checked under the
    /// lifecycle claim right before anything is moved out of it, because a
    /// `start --reuse` or a new session may have reopened or recreated the
    /// path since the checkout was judged: another open session at the
    /// path, a newer session recorded there, a live or unknown holder (when
    /// `check_holder`), a folder created after the session closed, or a
    /// start holding the folder's reservation. `None` lets the move go on.
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
        match self
            .store_ref()
            .latest_session_for_worktree_any_state(&session.worktree_path)
        {
            Ok(Some(latest)) if latest.id == session.id => {}
            Ok(_) => return Some("leftover_superseded"),
            Err(_) => return Some("open_session_unknown"),
        }
        if check_holder {
            let mut holders = crate::session_holder::RecordedHolderProcessTable::default();
            match crate::session_holder::recorded_holder_liveness(
                self.store_ref(),
                session.id,
                &mut holders,
                true,
            )
            .liveness
            {
                crate::session_holder::RecordedHolderLiveness::Unbound
                | crate::session_holder::RecordedHolderLiveness::Gone => {}
                crate::session_holder::RecordedHolderLiveness::Alive => {
                    return Some("holder_alive");
                }
                crate::session_holder::RecordedHolderLiveness::Unknown => {
                    return Some("holder_unknown");
                }
            }
        }
        let Some(closed_at) = session.closed_at.or(session.cleanup_completed_at) else {
            return Some("creation_time_unknown");
        };
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

    /// Strip catalogued regenerable directories from closed checkouts,
    /// oldest close first, at most `cap` checkouts, until `deadline`, with
    /// the evidence of `run` (open sessions' paths resolved once, one
    /// open-file observation). Never waits for a lock: a checkout whose
    /// lifecycle claim is busy is left for the next pass.
    pub(crate) fn strip_closed_checkouts(
        &self,
        run: &mut super::retire::RetireRun,
        deadline: Instant,
        cap: usize,
    ) -> CheckoutStripReport {
        let _span = crate::phase_telemetry::span("checkout_strip_pass");
        let mut report = CheckoutStripReport::default();
        let deadline = deadline.min(run.deadline());
        let candidates = match self
            .store_ref()
            .closed_spawned_sessions_oldest_closed_first(STRIP_CANDIDATES_MAX)
        {
            Ok(sessions) => sessions,
            Err(error) => {
                crate::warn_unrecorded::<(), _>("list closed checkouts to strip", Err(error));
                return report;
            }
        };
        let open = self
            .store_ref()
            .live_sessions()
            .map(|sessions| {
                sessions
                    .into_iter()
                    .filter(|session| session.cleanup_state == crate::SessionCleanupState::Open)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        report.backlog = self.observe_checkout_backlog(&candidates, &open, run);
        let done: BTreeMap<i64, i64> = self.meta_json(CHECKOUT_STRIP_DONE_KEY);
        let mut newly_done = BTreeMap::new();
        let mut holders = crate::session_holder::RecordedHolderProcessTable::default();
        // Each folder once per pass.
        let mut judged_paths = BTreeSet::new();
        for session in &candidates {
            if !judged_paths.insert(session.worktree_path.clone()) {
                continue;
            }
            let closed_at = session.closed_at.unwrap_or(0);
            if done.get(&session.id) == Some(&closed_at) {
                continue;
            }
            let worktree = Path::new(&session.worktree_path);
            match std::fs::symlink_metadata(worktree) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                _ => continue,
            }
            if report.checkouts_listed >= cap || Instant::now() >= deadline {
                report.more_remaining = true;
                break;
            }
            report.checkouts_examined += 1;
            match self.strip_one_checkout(session, run, &mut holders, deadline, &mut report) {
                CheckoutStrip::Done(directories) => {
                    if directories > 0 {
                        report.checkouts_stripped += 1;
                        report.directories_stripped += directories;
                    }
                    newly_done.insert(session.id, closed_at);
                }
                CheckoutStrip::Refused(code) => {
                    note_refusal(&mut report, code);
                    // Ownership never changes for a session: not asked again.
                    if code == "adopted" || code == "not_owned" {
                        newly_done.insert(session.id, closed_at);
                    }
                    if code == "process_use_unknown" {
                        report.process_use_unavailable = true;
                        report.more_remaining = true;
                        break;
                    }
                    if code == "deadline" {
                        report.more_remaining = true;
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
        if report.directories_stripped > 0 {
            crate::phase_telemetry::add("checkout_strip_checkouts_stripped", Duration::ZERO);
        }
        report
    }

    /// Strip the closed session `session_id`'s checkout at `finish`, under
    /// the same rules as the lane (a fresh open-file observation; a process
    /// still in the checkout, such as the finishing shell, keeps it for the
    /// lane). Returns the directories moved into the trash.
    pub(crate) fn strip_checkout_at_finish(&self, session_id: i64) -> Result<usize, String> {
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
        match self.strip_one_checkout(&session, &mut run, &mut holders, deadline, &mut report) {
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
        report: &mut CheckoutStripReport,
    ) -> CheckoutStrip {
        use CheckoutStrip::Refused;
        if Instant::now() >= deadline {
            return Refused("deadline");
        }
        let _git_deadline = crate::git::limit_git_until(deadline);
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
        match std::fs::symlink_metadata(&worktree) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => return Refused("checkout_missing"),
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
        match crate::session_holder::recorded_holder_liveness(
            self.store_ref(),
            session.id,
            holders,
            true,
        )
        .liveness
        {
            crate::session_holder::RecordedHolderLiveness::Unbound
            | crate::session_holder::RecordedHolderLiveness::Gone => {}
            crate::session_holder::RecordedHolderLiveness::Alive => {
                return Refused("holder_alive");
            }
            crate::session_holder::RecordedHolderLiveness::Unknown => {
                return Refused("holder_unknown");
            }
        }
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
        if candidates.is_empty() {
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
        if let Some(code) = self.reused_path_keep(&current, &worktree, true) {
            return Refused(code);
        }
        let mut stripped = 0_usize;
        for candidate in &candidates {
            if Instant::now() >= deadline {
                note_refusal(report, "deadline");
                break;
            }
            let prove_started = Instant::now();
            let proved = prove_strippable(&worktree, candidate, deadline);
            report.prove_ms = report.prove_ms.saturating_add(elapsed_ms(prove_started));
            if let Err(code) = proved {
                note_refusal(report, code);
                continue;
            }
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

    /// Count checkout folders from the broker's records: one `lstat` per
    /// folder and per top-level catalogued name, no byte walk.
    fn observe_checkout_backlog(
        &self,
        closed: &[Session],
        open: &[Session],
        run: &super::retire::RetireRun,
    ) -> CheckoutBacklog {
        let _span = crate::phase_telemetry::span("checkout_strip_backlog");
        let exists = |path: &str| {
            std::fs::symlink_metadata(path)
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        };
        let open_paths = open
            .iter()
            .map(|session| session.worktree_path.clone())
            .collect::<BTreeSet<_>>();
        let mut backlog = CheckoutBacklog {
            observed_at_ms: crate::clock::epoch_ms(),
            ..CheckoutBacklog::default()
        };
        let live = self.store_ref().live_sessions().unwrap_or_default();
        let mut folders = BTreeSet::new();
        for session in live.iter().chain(closed.iter()) {
            if !folders.contains(&session.worktree_path) && exists(&session.worktree_path) {
                folders.insert(session.worktree_path.clone());
            }
        }
        backlog.folders = folders.len();
        let mut closed_folders = BTreeSet::new();
        for session in closed {
            if open_paths.contains(&session.worktree_path)
                || closed_folders.contains(&session.worktree_path)
                || !folders.contains(&session.worktree_path)
                || run
                    .open_session_keep(session.id, Path::new(&session.worktree_path))
                    .is_some()
            {
                continue;
            }
            closed_folders.insert(session.worktree_path.clone());
        }
        backlog.closed_folders = closed_folders.len();
        backlog.closed_with_regenerable = closed_folders
            .iter()
            .filter(|path| {
                crate::artifact_catalog::CATALOG
                    .iter()
                    .filter(|kind| kind.unattended)
                    .any(|kind| exists(&Path::new(path).join(kind.name).to_string_lossy()))
            })
            .count();
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
        day.directories_stripped = day
            .directories_stripped
            .saturating_add(strip.directories_stripped);
        day.bytes_freed = day.bytes_freed.saturating_add(report.trash.stripped_bytes);
        while days.len() > STRIP_DAYS_KEPT {
            days.pop_first();
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
                ("discover".to_string(), strip.discover_ms),
                ("prove".to_string(), strip.prove_ms),
                ("move".to_string(), strip.move_ms),
            ]),
            backlog,
            bytes_freed_total: previous
                .bytes_freed_total
                .saturating_add(report.trash.stripped_bytes),
            days,
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
        assert!(
            regenerable_candidates(&root, far())
                .unwrap()
                .iter()
                .all(|candidate| candidate.relative != "node_modules")
        );
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
            StripKind::NodeModules.witness(&modules),
            Err("witness_missing")
        );
        std::fs::write(modules.join(".yarn-integrity"), "{}\n").unwrap();
        assert_eq!(StripKind::NodeModules.witness(&modules), Ok(()));
        let pnpm = at("b/node_modules");
        std::fs::create_dir_all(pnpm.join(".pnpm")).unwrap();
        assert_eq!(StripKind::NodeModules.witness(&pnpm), Ok(()));
        // A nested repository or submodule is never regenerable.
        std::fs::write(pnpm.join(".git"), "gitdir: elsewhere\n").unwrap();
        assert_eq!(
            StripKind::NodeModules.witness(&pnpm),
            Err("nested_repository")
        );

        let store = at(".pnpm-store");
        std::fs::create_dir_all(store.join("files")).unwrap();
        assert_eq!(StripKind::PnpmStore.witness(&store), Err("witness_missing"));
        std::fs::create_dir_all(store.join("v10")).unwrap();
        assert_eq!(StripKind::PnpmStore.witness(&store), Ok(()));

        let venv = at(".venv");
        std::fs::create_dir_all(venv.join("lib")).unwrap();
        assert_eq!(StripKind::Venv.witness(&venv), Err("witness_missing"));
        std::fs::write(venv.join("pyvenv.cfg"), "home = /usr\n").unwrap();
        assert_eq!(StripKind::Venv.witness(&venv), Ok(()));
        std::fs::write(venv.join("notes.txt"), "mine\n").unwrap();
        assert_eq!(StripKind::Venv.witness(&venv), Err("unexpected_content"));

        let target = at("target");
        std::fs::write(target.join("CACHEDIR.TAG"), "not a cache tag\n").unwrap();
        assert_eq!(
            StripKind::CargoTarget.witness(&target),
            Err("witness_missing")
        );
        std::fs::write(
            target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n# cargo\n",
        )
        .unwrap();
        assert_eq!(StripKind::CargoTarget.witness(&target), Ok(()));
        assert_eq!(StripKind::for_name("build"), None);
        assert_eq!(StripKind::for_name("dist"), None);
    }
}
