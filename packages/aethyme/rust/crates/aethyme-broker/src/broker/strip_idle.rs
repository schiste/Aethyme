//! Strip catalogued regenerable directories out of idle open sessions.
//!
//! A session left open with nobody working in it keeps its dependency
//! trees (often most of the checkout's size) until it is closed, which for
//! work that was never pushed takes days. Those directories are
//! regenerable by definition, so they need not wait: after
//! `[cleanup] idle_strip_after_hours` (default 4, `0` off) without session
//! activity, the same catalogue, proofs, witnesses and trash path as the
//! closed-checkout strip ([`super::strip`]) apply to the open session's
//! checkout.
//!
//! Idle means all of:
//!
//! - no session activity for that long;
//! - no process with its working directory or an open file inside the
//!   checkout, by a complete open-file observation (an incomplete one stops
//!   the pass);
//! - no unresolved coordinated operation and no running gate;
//! - no start holding the folder's reservation;
//! - no recent file-system activity: the modification times of the
//!   checkout's Git index and `HEAD`, of the checkout's top-level directory
//!   and of its tracked top-level directories (a handful of `stat` calls, no
//!   walk) are all older than the threshold. An agent working by absolute
//!   paths from elsewhere leaves no session activity but does leave these.
//!
//! A live holder alone does not block: the directories are regenerable and
//! the open-file check sees whatever still uses them. Under the lifecycle
//! claim, right before anything moves, the session must still be open at the
//! same path with the same last activity, the reservation is asked again,
//! the checkout's artifact-use lock (the one `submit prepare` holds shared)
//! is taken exclusively, and the open-file observation is taken again when
//! the run's is older than [`PROCESS_USE_FRESH_FOR`].
//!
//! Only what the session's own preparation recreates is stripped from an
//! open session, in three cases (see [`idle_coverage`]): a directory a step
//! declares as an output exactly; a Python environment root (the
//! closed-checkout strip's `.venv` witness: `pyvenv.cfg` and nothing outside
//! the environment layout) whose interpreter a step declares as an output
//! (`<root>/pyvenv.cfg`, or `<root>/bin/python*`, `<root>/Scripts/python*`),
//! which proves that step creates it (a root holding other declared files
//! but no declared interpreter is kept as
//! `environment_root_creation_unproven`: its step may only install into an
//! environment made elsewhere); and a workspace member's
//! `node_modules` the closed-checkout strip proves is a pnpm member (its
//! links into the top-level store), when the top-level `node_modules` is a
//! declared output and actually moved in the same pass or is already absent, so
//! the declared install recreates root and members together. Every candidate
//! still goes through the closed-checkout strip's proofs; what they refuse
//! (`witness_missing`, `unexpected_content`, ...) is kept with the same
//! reason. Members whose top-level `node_modules` stays are kept as
//! `member_root_kept`. `submit prepare` recreates all of those, so the
//! one-command promise holds. Every other catalogued directory is kept and
//! counted as `not_declared_output`. (The closed-checkout strip is
//! unchanged.)
//!
//! After a strip the session's preparation reads stale (see
//! `preparation::mark_stripped`) with its usual `next_action`, and the
//! session gets one note naming what was removed and how to prepare again.
//! A session is stripped once per stretch of inactivity: it is armed again
//! only by new activity (its `last_activity_at` changes).
//!
//! Once the session has had no activity for the host's artifact max age
//! (`[budget] artifact_max_age_hours` in the host's `storage.toml`, default
//! 24, `0` off), its Cargo `target` directories go too, declared or not
//! ([`rebuilt_on_demand`]): Cargo rebuilds a target on the next build, so no
//! preparation step is needed. Every proof above still applies. A session
//! idle-stripped earlier is examined once more when it reaches the max age
//! ([`IDLE_STRIP_MAX_AGE_DONE_KEY`]). When only such targets were removed,
//! preparation is not marked stale and no notice is left.

use super::strip::{CheckoutStripReport, StripTarget};
use super::*;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Meta key: open sessions the idle strip handled, with the
/// `last_activity_at` it saw (JSON: session id to that time); a session is
/// looked at again only once its activity moved.
pub(crate) const IDLE_STRIP_DONE_KEY: &str = "checkout_strip.idle_done";
/// Meta key: the coverage rules the memo in [`IDLE_STRIP_DONE_KEY`] was
/// written under. A memo from older rules is set aside, so every idle
/// session is examined once more under the current rules (one that a
/// narrower rule left with directories it may now remove).
pub(crate) const IDLE_STRIP_RULES_KEY: &str = "checkout_strip.idle_done_rules";
/// The current coverage rules; bump it whenever what the idle strip may
/// remove from an open session widens. 2: environment roots whose
/// interpreter a step declares, and pnpm workspace members with their root.
/// 3: environment roots a recorded preparation created, and receipts of
/// commands that never settled no longer keep a session idle for the
/// artifact max age.
pub(crate) const IDLE_STRIP_RULES: u32 = 3;
/// Meta key: open sessions the idle strip handled once idle for the artifact
/// max age (JSON: session id to the `last_activity_at` it saw).
pub(crate) const IDLE_STRIP_MAX_AGE_DONE_KEY: &str = "checkout_strip.idle_max_age_done";
/// Meta key: open sessions the idle strip refused for a reason that lasts
/// (JSON: session id to `[last_activity_at, refused_at_ms]`). Such a session
/// is not examined again for [`IDLE_STRIP_REFUSAL_BACKOFF_MS`] unless its
/// activity moves, so the oldest sessions, which sort first, cannot keep the
/// whole pass to themselves (on the busiest repository 143 passes in a day
/// re-examined the same refused sessions and stripped none).
pub(crate) const IDLE_STRIP_REFUSED_KEY: &str = "checkout_strip.idle_refused";
/// Meta key: idle open sessions still waiting after the last pass (JSON
/// integer); the retirement run reads it to give a large backlog a larger
/// share of its window.
pub(crate) const IDLE_STRIP_BACKLOG_KEY: &str = "checkout_strip.idle_backlog";
/// Meta key: bytes of regenerable directories measured in an open session's
/// checkout (JSON: session id to bytes, `0` once stripped).
pub(crate) const IDLE_STRIP_SAMPLES_KEY: &str = "checkout_strip.idle_samples";
/// Meta key: the last [`CheckoutByteEstimate`] (JSON), shown by `storage
/// status`.
pub(crate) const IDLE_STRIP_ESTIMATE_KEY: &str = "checkout_strip.byte_estimate";
/// How long a lasting refusal keeps a session from being examined again.
const IDLE_STRIP_REFUSAL_BACKOFF_MS: i64 = 2 * 3_600_000;
/// Refusals that outlast a pass and cost time to reach (the session is
/// examined again once the backoff ends, or its activity moves). Every other
/// refusal (a process in the checkout, a preparation running, recent file
/// activity, a transient one) is judged again at the next pass.
const BACKOFF_REFUSALS: &[&str] = &[
    "operation_unresolved",
    "operations_unknown",
    "gates_unknown",
    "activity_unknown",
    "checkout_missing",
    "discovery_failed",
];
/// Checkouts examined per pass while foreground work (an install, a gate)
/// writes on the checkouts' volume: the strip competes with it for the disk.
const IDLE_STRIP_FOREGROUND_CAP: usize = 3;
/// Remembered measurements at most; the lowest ids go first.
const IDLE_STRIP_SAMPLES_MAX: usize = 16_384;
/// Remembered sessions at most; the lowest ids go first.
const IDLE_STRIP_DONE_MAX: usize = 16_384;
/// Open sessions the idle strip lists per run, at most (each costs a few
/// cheap reads and O(1) renames).
pub(crate) const IDLE_STRIP_CHECKOUTS_PER_RUN: usize = 100;
/// The longest one checkout may take.
const IDLE_STRIP_CHECKOUT_BUDGET: Duration = Duration::from_secs(15);
/// The run's open-file observation is used as is while younger than this;
/// otherwise a fresh one is taken under the claim right before the moves.
pub(crate) const PROCESS_USE_FRESH_FOR: Duration = Duration::from_secs(5);
/// [`recent_file_activity`]'s answer when the changed-file probe could not
/// complete (timeout, failure, or its cap reached without evidence).
pub(crate) const ACTIVITY_UNKNOWN: &str = "activity_unknown";
/// Tracked top-level directories whose modification time is read, at most.
const ACTIVITY_TOP_LEVEL_DIRECTORIES: usize = 32;
/// The fingerprint of the note left after an idle strip.
pub(crate) const IDLE_STRIP_NOTE_FINGERPRINT: &str = "idle-regenerable-strip";

/// What the idle strip pass did, beside the directory-level counters of
/// its [`CheckoutStripReport`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct IdleStripReport {
    pub strip: CheckoutStripReport,
    /// Open sessions idle past the threshold whose current stretch of
    /// inactivity has not been handled yet (the backlog).
    pub idle_backlog: usize,
    /// Of the idle sessions, skipped because this stretch was handled.
    pub skipped_handled: usize,
    /// Sessions given the note this run.
    pub noted: usize,
    /// Idle sessions not examined because a lasting refusal is still
    /// within its backoff.
    pub skipped_backoff: usize,
    /// Bytes of regenerable directories measured per examined session this
    /// pass (what stayed after the strip).
    pub samples: BTreeMap<i64, u64>,
    /// Idle sessions kept because a recent file-system modification time
    /// shows someone working in the checkout.
    pub kept_recent_files: usize,
    /// Catalogued directories kept because no preparation step declares
    /// them as an output.
    pub kept_not_declared: usize,
    /// Milliseconds spent measuring bytes this pass (bounded by
    /// [`IDLE_STRIP_MEASURE_PASS_BUDGET`]).
    pub measure_ms: u64,
    /// Per kind (`node_modules`, `node_modules_member`, `venv`,
    /// `pnpm_store`, `cargo_target`): directories removed and kept, and
    /// their bytes as far as the bounded measurement got.
    pub by_kind: BTreeMap<&'static str, KindTally>,
}

/// Removed and kept directories of one kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct KindTally {
    pub removed: usize,
    pub removed_bytes: u64,
    pub kept: usize,
    pub kept_bytes: u64,
    /// Directories whose bytes were not measured (budget spent).
    pub unmeasured: usize,
}

/// The longest one checkout's byte measurement may take.
const IDLE_STRIP_MEASURE_BUDGET: Duration = Duration::from_secs(1);
/// The longest all byte measurements of one pass may take together (the
/// measurement is telemetry; the time belongs to the strip).
const IDLE_STRIP_MEASURE_PASS_BUDGET: Duration = Duration::from_secs(3);

/// How a catalogued directory is covered by the session's preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coverage {
    /// A step output names it exactly.
    Exact,
    /// A Python environment root whose interpreter a step declares as an
    /// output (`<root>/pyvenv.cfg` or `<root>/bin/python*`, or
    /// `<root>/Scripts/python*`): that step creates the environment.
    EnvironmentRoot,
    /// A Python environment root containing a declared output path, but no
    /// step declares its interpreter: a step may only install into an
    /// environment made elsewhere, so it is kept
    /// (`environment_root_creation_unproven`).
    EnvironmentRootUnproven,
    /// A workspace member's `node_modules` while the top-level
    /// `node_modules` is a declared output.
    WorkspaceMember,
}

/// Whether `candidate`, which no step declares, may still go from a session
/// idle for the artifact max age (`past_max_age`): a Cargo target, which
/// Cargo rebuilds on the next build.
pub(crate) fn rebuilt_on_demand(
    candidate: &super::strip::StripCandidate,
    past_max_age: bool,
) -> bool {
    past_max_age && candidate.kind == super::strip::StripKind::CargoTarget
}

/// The host's artifact max age in milliseconds, `None` when it is off or
/// the host state directory is unknown.
pub(super) fn artifact_max_age_ms(host_state: Option<&Path>) -> Option<i64> {
    let (budgets, _) = crate::storage_ledger::Budgets::load(host_state?);
    budgets
        .artifact_max_age()
        .map(|age| i64::try_from(age.as_millis()).unwrap_or(i64::MAX))
}

fn normalised_outputs(outputs: &[String]) -> Vec<String> {
    outputs
        .iter()
        .map(|output| {
            output
                .replace('\\', "/")
                .trim_start_matches("./")
                .trim_end_matches('/')
                .to_string()
        })
        .filter(|output| !output.is_empty())
        .collect()
}

/// Whether, and how, the declared step `outputs` cover `candidate` for an
/// open session (see the module documentation). `None`: not covered, kept
/// as `not_declared_output`.
pub(crate) fn idle_coverage(
    outputs: &[String],
    candidate: &super::strip::StripCandidate,
) -> Option<Coverage> {
    use super::strip::StripKind;
    let relative = candidate.relative.as_str();
    if declared_output_covers(outputs, relative) {
        return Some(Coverage::Exact);
    }
    let outputs = normalised_outputs(outputs);
    if candidate.kind == StripKind::Venv {
        let inside: Vec<&str> = outputs
            .iter()
            .filter_map(|output| output.strip_prefix(relative))
            .filter_map(|rest| rest.strip_prefix('/'))
            .collect();
        if inside.iter().any(|rest| {
            *rest == "pyvenv.cfg"
                || rest
                    .strip_prefix("bin/")
                    .or_else(|| rest.strip_prefix("Scripts/"))
                    .is_some_and(|name| name.starts_with("python"))
        }) {
            return Some(Coverage::EnvironmentRoot);
        }
        if !inside.is_empty() {
            return Some(Coverage::EnvironmentRootUnproven);
        }
    }
    if candidate.kind == StripKind::NodeModules
        && relative.contains('/')
        && outputs.iter().any(|output| output == "node_modules")
    {
        return Some(Coverage::WorkspaceMember);
    }
    None
}

/// The telemetry kind of a candidate.
fn kind_label(candidate: &super::strip::StripCandidate) -> &'static str {
    use super::strip::StripKind;
    match candidate.kind {
        StripKind::NodeModules if candidate.relative.contains('/') => "node_modules_member",
        StripKind::NodeModules => "node_modules",
        StripKind::PnpmStore => "pnpm_store",
        StripKind::Venv => "venv",
        StripKind::CargoTarget => "cargo_target",
    }
}

fn removed_counter(label: &str) -> &'static str {
    match label {
        "node_modules_member" => "checkout_idle_strip_removed_node_modules_member",
        "node_modules" => "checkout_idle_strip_removed_node_modules",
        "pnpm_store" => "checkout_idle_strip_removed_pnpm_store",
        "venv" => "checkout_idle_strip_removed_venv",
        _ => "checkout_idle_strip_removed_cargo_target",
    }
}

fn kept_counter(label: &str) -> &'static str {
    match label {
        "node_modules_member" => "checkout_idle_strip_kept_node_modules_member",
        "node_modules" => "checkout_idle_strip_kept_node_modules",
        "pnpm_store" => "checkout_idle_strip_kept_pnpm_store",
        "venv" => "checkout_idle_strip_kept_venv",
        _ => "checkout_idle_strip_kept_cargo_target",
    }
}

/// The phase counter of one checkout-level idle strip refusal.
fn idle_refused_counter(code: &str) -> &'static str {
    match code {
        "not_owned" => "checkout_idle_strip_refused_not_owned",
        "preparation_not_configured" => "checkout_idle_strip_refused_preparation_not_configured",
        "checkout_missing" => "checkout_idle_strip_refused_checkout_missing",
        "operation_unresolved" => "checkout_idle_strip_refused_operation_unresolved",
        "operations_unknown" => "checkout_idle_strip_refused_operations_unknown",
        "gate_running" => "checkout_idle_strip_refused_gate_running",
        "gates_unknown" => "checkout_idle_strip_refused_gates_unknown",
        "start_reserved" => "checkout_idle_strip_refused_start_reserved",
        "recent_file_activity" => "checkout_idle_strip_refused_recent_file_activity",
        "activity_unknown" => "checkout_idle_strip_refused_activity_unknown",
        "checkout_busy" => "checkout_idle_strip_refused_checkout_busy",
        "checkout_marker_live" => "checkout_idle_strip_refused_checkout_marker_live",
        "checkout_lock_unknown" => "checkout_idle_strip_refused_checkout_lock_unknown",
        "discovery_failed" => "checkout_idle_strip_refused_discovery_failed",
        "process_using_checkout" => "checkout_idle_strip_refused_process_using_checkout",
        "process_use_unknown" => "checkout_idle_strip_refused_process_use_unknown",
        "fresh_process_use_unknown" => "checkout_idle_strip_refused_fresh_process_use_unknown",
        "lifecycle_busy" => "checkout_idle_strip_refused_lifecycle_busy",
        "changed" => "checkout_idle_strip_refused_changed",
        "deadline" => "checkout_idle_strip_refused_deadline",
        _ => "checkout_idle_strip_refused_other",
    }
}

fn note_idle_refusal(report: &mut IdleStripReport, code: &'static str) {
    *report.strip.refused.entry(code.to_string()).or_default() += 1;
    crate::phase_telemetry::add(idle_refused_counter(code), Duration::ZERO);
}

/// One idle checkout's outcome.
enum IdleStrip {
    /// These relative paths moved into the trash (none: nothing to strip),
    /// and of those, the ones Cargo rebuilds on demand ([`rebuilt_on_demand`]).
    Done(Vec<String>, Vec<String>),
    Refused(&'static str),
}

/// The note left for a session whose directories the idle strip removed.
pub(crate) fn idle_strip_note(session_id: i64, stripped: &[String], idle_hours: i64) -> String {
    format!(
        "Session {session_id} had no activity for {idle_hours} hour(s) and no process in its \
         checkout, so its regenerable dependency directories were moved out for deletion: {}. \
         Nothing else changed: commits, staged and unstaged changes and every other file stay. \
         Its preparation now reads stale; run `aethyme broker submit prepare --session \
         {session_id}` before building or testing again.",
        stripped.join(", ")
    )
}

/// What became of the idle strip's notice in one checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeOutcome {
    Written,
    /// An idle strip notice was already there; it already names the command.
    AlreadyPresent,
    /// Another notice (a conflict notice) is there; it is never overwritten.
    OtherNotice,
    /// Git would see the file: it is not ignored.
    NotIgnored,
    /// The path is tracked: never written, whatever the ignore rules say.
    Tracked,
    Failed,
}

impl NoticeOutcome {
    fn counter(self) -> &'static str {
        match self {
            Self::Written => "checkout_idle_strip_notice_written",
            Self::AlreadyPresent => "checkout_idle_strip_notice_already_present",
            Self::OtherNotice => "checkout_idle_strip_notice_skipped_other",
            Self::NotIgnored => "checkout_idle_strip_notice_skipped_not_ignored",
            Self::Tracked => "checkout_idle_strip_notice_skipped_tracked",
            Self::Failed => "checkout_idle_strip_notice_failed",
        }
    }
}

/// Run one bounded Git query in `worktree`; whether it exited 0.
fn git_succeeds(worktree: &Path, args: &[&str]) -> bool {
    let mut command = crate::git::git_command();
    command.args(args).current_dir(worktree);
    crate::bounded_output::output_limited(&mut command, crate::git::git_timeout(), 4096).is_ok_and(
        |output| {
            matches!(output, crate::bounded_output::LimitedOutput::Completed(output) if output.status.success())
        },
    )
}

/// Leave the idle strip's notice at `.aethyme/broker-action-required.md` in
/// `worktree`, the file agents are told to read first. It is created only
/// when absent (`create_new`): another notice (a conflict notice) is never
/// overwritten, not even by a check-then-write race. It is never written to
/// a tracked path, and only where the checkout's ignore rules cover it (the
/// notice never shows up in `git status`). When it is not written, the
/// session-scoped blocker and the status advice still carry the command.
pub(crate) fn write_idle_strip_notice(
    worktree: &Path,
    session_id: i64,
    stripped: &[String],
    idle_hours: i64,
) -> NoticeOutcome {
    write_prepare_notice(
        worktree,
        session_id,
        &format!(
            "Session {session_id} had no activity for {idle_hours} hour(s) and no process in this \
             checkout, so its regenerable dependency directories were moved out for deletion:"
        ),
        stripped,
    )
}

/// The same notice after a restore from a recovery archive: regenerable
/// directories are never archived, so the restored checkout lacks them.
pub(crate) fn write_restore_notice(
    worktree: &Path,
    session_id: i64,
    missing: &[String],
) -> NoticeOutcome {
    write_prepare_notice(
        worktree,
        session_id,
        &format!(
            "Session {session_id}'s checkout was restored from its recovery archive. Regenerable \
             dependency directories are never archived, so they are missing:"
        ),
        missing,
    )
}

/// Write the notice that dependencies are missing and one command rebuilds
/// them (see [`write_idle_strip_notice`] for where and when it is written).
fn write_prepare_notice(
    worktree: &Path,
    session_id: i64,
    explanation: &str,
    removed: &[String],
) -> NoticeOutcome {
    let relative = crate::merge::ACTION_REQUIRED_RELPATH;
    let path = worktree.join(relative);
    if path.exists() {
        return if crate::preparation::is_idle_strip_notice(&path) {
            NoticeOutcome::AlreadyPresent
        } else {
            NoticeOutcome::OtherNotice
        };
    }
    // `check-ignore --no-index` calls a tracked-and-ignored path ignored, so
    // the tracked state is asked on its own first.
    if git_succeeds(worktree, &["ls-files", "--error-unmatch", "--", relative]) {
        return NoticeOutcome::Tracked;
    }
    if !git_succeeds(
        worktree,
        &["check-ignore", "-q", "--no-index", "--", relative],
    ) {
        return NoticeOutcome::NotIgnored;
    }
    let body = format!(
        "{}\n# Dependencies were removed from this checkout\n\n{explanation}\n\n{}\n\n\
         Your work is untouched: commits, staged and unstaged changes and every other file are \
         where you left them.\n\n\
         Rebuild the dependencies with one command before building or testing:\n\n    {}\n\n\
         This notice disappears once the session is prepared again.\n",
        crate::preparation::IDLE_STRIP_NOTICE_HEADER,
        removed
            .iter()
            .map(|relative| format!("- `{relative}`"))
            .collect::<Vec<_>>()
            .join("\n"),
        crate::preparation::prepare_command(session_id),
    );
    let created = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
        });
    match created {
        Ok(mut file) => {
            use std::io::Write as _;
            if file.write_all(body.as_bytes()).is_ok() {
                NoticeOutcome::Written
            } else {
                NoticeOutcome::Failed
            }
        }
        // Someone wrote a notice between the check and the create: theirs
        // stays.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            NoticeOutcome::OtherNotice
        }
        Err(_) => NoticeOutcome::Failed,
    }
}

/// Whether a step output covers `relative` (slash-separated, relative to
/// the checkout): it names exactly that directory, so `submit prepare`
/// rebuilds exactly what the strip removed. A directory merely inside a
/// declared output is kept: the step may rebuild only part of its output.
pub(crate) fn declared_output_covers(outputs: &[String], relative: &str) -> bool {
    outputs.iter().any(|output| {
        let output = output.replace('\\', "/");
        let output = output.trim_start_matches("./").trim_end_matches('/');
        !output.is_empty() && relative == output
    })
}

/// The Git administrative directory of `worktree`: `.git` itself, or the
/// directory a linked worktree's `.git` file points to.
fn git_admin_dir(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    let metadata = std::fs::symlink_metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        return Some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = Path::new(text.lines().next()?.strip_prefix("gitdir:")?.trim());
    Some(if target.is_absolute() {
        target.to_path_buf()
    } else {
        worktree.join(target)
    })
}

/// The tracked top-level directories of `worktree` (at `HEAD`), at most
/// [`ACTIVITY_TOP_LEVEL_DIRECTORIES`].
fn tracked_top_level_directories(worktree: &Path) -> Vec<String> {
    let mut command = crate::git::git_command();
    command
        .args(["ls-tree", "-d", "-z", "--name-only", "HEAD"])
        .current_dir(worktree);
    match crate::bounded_output::output_limited(&mut command, crate::git::git_timeout(), 64 * 1024)
    {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) if output.status.success() => {
            output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|name| !name.is_empty())
                .filter_map(|name| std::str::from_utf8(name).ok().map(str::to_owned))
                .take(ACTIVITY_TOP_LEVEL_DIRECTORIES)
                .collect()
        }
        _ => Vec::new(),
    }
}

/// File-system evidence that someone worked in `worktree` since `since`:
/// which modification time is newer (the Git index or `HEAD`, the checkout
/// directory, or a tracked top-level directory), or `None`.
pub(crate) fn recent_file_activity(
    worktree: &Path,
    since: std::time::SystemTime,
    deadline: Instant,
) -> Option<&'static str> {
    let newer = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified >= since)
    };
    let index_head_decides = || {
        crate::worktree_status_activity::note_index_head_decides(
            crate::worktree_status_activity::IDLE_STRIP_LOOK,
        );
    };
    if let Some(admin) = git_admin_dir(worktree) {
        if newer(&admin.join("index")) {
            index_head_decides();
            return Some("git_index");
        }
        if newer(&admin.join("HEAD")) {
            index_head_decides();
            return Some("git_head");
        }
    }
    if newer(worktree) {
        return Some("checkout_directory");
    }
    if tracked_top_level_directories(worktree)
        .iter()
        .any(|name| newer(&worktree.join(name)))
    {
        return Some("tracked_directory");
    }
    // A deep edit touches none of the above: the changed files' own times
    // (and their parents'). A probe that cannot tell (timeout, or its cap
    // reached without evidence) leaves the answer to the checks above.
    // A probe that could not complete keeps the session: nothing is
    // stripped on evidence that was not read.
    match crate::worktree_status_activity::recent_status_activity(worktree, since) {
        Some(true) => Some("changed_file"),
        Some(false) => None,
        // `git status` could not finish (a large checkout on a slow
        // volume): a bounded walk of the checkout's modification times
        // decides instead; one that cannot finish keeps the session.
        // Only what the strip catalogue itself accepts as regenerable (its
        // witness) is left out of the walk; within the checkout's deadline.
        None => match crate::worktree_status_activity::recent_mtime_walk(
            worktree,
            since,
            deadline.min(Instant::now() + crate::worktree_status_activity::MTIME_WALK_BUDGET),
            crate::worktree_status_activity::MTIME_WALK_ENTRIES,
            &|path: &Path, name: &str| {
                super::strip::StripKind::for_name(name)
                    .is_some_and(|kind| kind.witness(path, &[]).is_ok())
            },
        ) {
            Some(true) => Some("changed_file_walk"),
            Some(false) => None,
            None => Some(ACTIVITY_UNKNOWN),
        },
    }
}

/// What the open sessions' checkouts hold in regenerable directories, as far
/// as the idle strip's paced measurements reach (see
/// [`estimate_open_checkout_bytes`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CheckoutByteEstimate {
    /// The worktree root (the directory holding the checkouts).
    pub worktree_root: String,
    /// Open sessions of this repository.
    pub open_checkouts: usize,
    /// Of those, the ones with a measurement.
    pub sampled: usize,
    /// Bytes measured across the sampled ones.
    pub measured_bytes: u64,
    /// `measured_bytes` plus the mean of the sampled checkouts still holding
    /// directories for every checkout not sampled.
    pub estimated_bytes: u64,
    pub observed_at_ms: i64,
}

/// The estimate for `open` sessions from `samples` (session id to measured
/// bytes, `0` for a stripped one). A session without a measurement counts
/// as the mean of the measured sessions that still hold bytes: the sessions
/// examined first are the oldest, so a stripped one is no sign of what the
/// rest hold.
pub(crate) fn estimate_open_checkout_bytes(
    open: &[i64],
    samples: &BTreeMap<i64, u64>,
) -> CheckoutByteEstimate {
    let measured: Vec<u64> = open
        .iter()
        .filter_map(|id| samples.get(id))
        .copied()
        .collect();
    let holding: Vec<u64> = measured
        .iter()
        .copied()
        .filter(|bytes| *bytes > 0)
        .collect();
    let measured_bytes = measured
        .iter()
        .fold(0_u64, |sum, bytes| sum.saturating_add(*bytes));
    let mean = if holding.is_empty() {
        0
    } else {
        holding
            .iter()
            .fold(0_u64, |sum, bytes| sum.saturating_add(*bytes))
            / u64::try_from(holding.len()).unwrap_or(u64::MAX).max(1)
    };
    let unsampled = u64::try_from(open.len().saturating_sub(measured.len())).unwrap_or(u64::MAX);
    CheckoutByteEstimate {
        worktree_root: String::new(),
        open_checkouts: open.len(),
        sampled: measured.len(),
        measured_bytes,
        estimated_bytes: measured_bytes.saturating_add(mean.saturating_mul(unsampled)),
        observed_at_ms: 0,
    }
}

/// Whether the Python environment at `relative` was created by the
/// preparation that ran in `window` (epoch ms, start and completion): its
/// `pyvenv.cfg`, which only creating the environment writes, is dated inside
/// it. A step that declares files inside the environment but not its
/// interpreter may only install into an environment made elsewhere; one that
/// created it can create it again.
pub(crate) fn environment_created_in(worktree: &Path, relative: &str, window: (i64, i64)) -> bool {
    const SLACK_MS: i64 = 2_000;
    std::fs::metadata(worktree.join(relative).join("pyvenv.cfg"))
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|since| i64::try_from(since.as_millis()).ok())
        .is_some_and(|modified| {
            modified >= window.0.saturating_sub(SLACK_MS)
                && modified <= window.1.saturating_add(SLACK_MS)
        })
}

impl Broker {
    /// Strip catalogued regenerable directories from open sessions idle
    /// past `[cleanup] idle_strip_after_hours`, oldest activity first, at
    /// most `cap` checkouts, until `deadline`, with the evidence of `run`
    /// (one open-file observation for the whole pass). See the module
    /// documentation for the rules.
    pub(crate) fn strip_idle_open_checkouts(
        &mut self,
        run: &mut super::retire::RetireRun,
        deadline: Instant,
        cap: usize,
    ) -> IdleStripReport {
        let _span = crate::phase_telemetry::span("checkout_idle_strip_pass");
        let mut report = IdleStripReport::default();
        let idle_after_ms = crate::auto_cleanup::idle_strip_after_ms(self.main_root());
        if idle_after_ms <= 0 {
            crate::phase_telemetry::add("checkout_idle_strip_off", Duration::ZERO);
            return report;
        }
        let deadline = deadline.min(run.deadline());
        let now_ms = crate::clock::epoch_ms();
        let open: Vec<Session> = match self.store_ref().live_sessions() {
            Ok(sessions) => sessions
                .into_iter()
                .filter(|session| {
                    session.cleanup_state == crate::SessionCleanupState::Open
                        && session.origin == crate::SessionOrigin::Spawned
                })
                .collect(),
            Err(error) => {
                crate::warn_unrecorded::<(), _>("list idle open sessions", Err(error));
                return report;
            }
        };
        let mut idle = open
            .iter()
            .filter(|session| now_ms.saturating_sub(session.last_activity_at) >= idle_after_ms)
            .cloned()
            .collect::<Vec<_>>();
        idle.sort_by_key(|session| (session.last_activity_at, session.id));
        // Foreground work (an install, a gate) writing on the checkouts'
        // volume has the disk first: examine only a few this pass.
        let cap = {
            let _span = crate::phase_telemetry::span("checkout_idle_strip_foreground_check");
            let device = idle
                .first()
                .and_then(|session| Path::new(&session.worktree_path).parent())
                .and_then(crate::foreground_io::device_of);
            if device.is_some_and(|device| crate::foreground_io::active_on(&[device])) {
                crate::phase_telemetry::add(
                    "checkout_idle_strip_yielded_to_foreground",
                    Duration::ZERO,
                );
                cap.min(IDLE_STRIP_FOREGROUND_CAP)
            } else {
                cap
            }
        };
        // A memo written under older coverage rules is set aside: every
        // idle session is examined once more under the current ones.
        let rules: u32 = self.meta_json(IDLE_STRIP_RULES_KEY);
        let memo_outdated = rules != IDLE_STRIP_RULES;
        let handled: BTreeMap<i64, i64> = if memo_outdated {
            crate::phase_telemetry::add("checkout_idle_strip_memo_outdated", Duration::ZERO);
            BTreeMap::new()
        } else {
            self.meta_json(IDLE_STRIP_DONE_KEY)
        };
        let venv_markers = crate::auto_cleanup::venv_markers(self.main_root());
        let idle_hours = idle_after_ms / 3_600_000;
        let max_age_ms = artifact_max_age_ms(self.storage_context().host_state_dir.as_deref());
        let handled_max_age: BTreeMap<i64, i64> = if max_age_ms.is_some() && !memo_outdated {
            self.meta_json(IDLE_STRIP_MAX_AGE_DONE_KEY)
        } else {
            BTreeMap::new()
        };
        let refused_before: BTreeMap<i64, (i64, i64)> = self.meta_json(IDLE_STRIP_REFUSED_KEY);
        let mut newly_refused: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
        let mut cleared_refusals: Vec<i64> = Vec::new();
        let mut newly_handled = BTreeMap::new();
        let mut newly_handled_max_age = BTreeMap::new();
        let mut listed = 0_usize;
        for session in &idle {
            let past_max_age = max_age_ms
                .is_some_and(|age| now_ms.saturating_sub(session.last_activity_at) >= age);
            let handled_now = if past_max_age {
                handled_max_age.get(&session.id) == Some(&session.last_activity_at)
            } else {
                handled.get(&session.id) == Some(&session.last_activity_at)
            };
            if handled_now {
                report.skipped_handled += 1;
                continue;
            }
            if refused_before
                .get(&session.id)
                .is_some_and(|(activity, at)| {
                    *activity == session.last_activity_at
                        && now_ms.saturating_sub(*at) < IDLE_STRIP_REFUSAL_BACKOFF_MS
                })
            {
                report.skipped_backoff += 1;
                crate::phase_telemetry::add("checkout_idle_strip_skipped_backoff", Duration::ZERO);
                continue;
            }
            report.idle_backlog += 1;
            if listed >= cap || Instant::now() >= deadline {
                report.strip.more_remaining = true;
                continue;
            }
            listed += 1;
            report.strip.checkouts_examined += 1;
            crate::phase_telemetry::add("checkout_idle_strip_examined", Duration::ZERO);
            let checkout_deadline = deadline.min(Instant::now() + IDLE_STRIP_CHECKOUT_BUDGET);
            let transient_before = super::strip::transient_refusals(&report.strip);
            match self.strip_idle_checkout(
                session,
                run,
                checkout_deadline,
                &venv_markers,
                max_age_ms
                    .filter(|_| past_max_age)
                    .map(|age| now_ms.saturating_sub(age)),
                &mut report,
            ) {
                IdleStrip::Done(stripped, on_demand) => {
                    if transient_before == super::strip::transient_refusals(&report.strip) {
                        cleared_refusals.push(session.id);
                        newly_handled.insert(session.id, session.last_activity_at);
                        if past_max_age {
                            newly_handled_max_age.insert(session.id, session.last_activity_at);
                        }
                    }
                    if stripped.is_empty() {
                        crate::phase_telemetry::add(
                            "checkout_idle_strip_nothing_found",
                            Duration::ZERO,
                        );
                        continue;
                    }
                    report.strip.checkouts_stripped += 1;
                    report.strip.directories_stripped += stripped.len();
                    report.idle_backlog = report.idle_backlog.saturating_sub(1);
                    crate::phase_telemetry::add(
                        "checkout_idle_strip_stripped_checkout",
                        Duration::ZERO,
                    );
                    if stripped.len() == on_demand.len() {
                        // Only Cargo targets: Cargo rebuilds them, so the
                        // preparation stays fresh and no notice is needed.
                        crate::phase_telemetry::add(
                            "checkout_idle_strip_max_age_targets_only",
                            Duration::ZERO,
                        );
                        continue;
                    }
                    crate::warn_unrecorded(
                        "mark an idle-stripped session's preparation stale",
                        crate::preparation::mark_stripped(self.main_root(), session.id, &stripped)
                            .map_err(|error| error.to_string()),
                    );
                    let notice = write_idle_strip_notice(
                        Path::new(&session.worktree_path),
                        session.id,
                        &stripped,
                        idle_hours,
                    );
                    crate::phase_telemetry::add(notice.counter(), Duration::ZERO);
                    let note = idle_strip_note(session.id, &stripped, idle_hours);
                    match self.store().record_automation_note(
                        crate::session_auto_close::NOTE_SENDER,
                        session.id,
                        &note,
                        Some(IDLE_STRIP_NOTE_FINGERPRINT),
                    ) {
                        Ok(_) => {
                            report.noted += 1;
                            crate::phase_telemetry::add(
                                "checkout_idle_strip_noted",
                                Duration::ZERO,
                            );
                        }
                        Err(error) => {
                            crate::phase_telemetry::add(
                                "checkout_idle_strip_note_failed",
                                Duration::ZERO,
                            );
                            crate::warn_unrecorded::<(), _>(
                                "leave the idle strip note",
                                Err(error),
                            );
                        }
                    }
                }
                IdleStrip::Refused(code) => {
                    note_idle_refusal(&mut report, code);
                    if BACKOFF_REFUSALS.contains(&code) {
                        newly_refused.insert(session.id, (session.last_activity_at, now_ms));
                    }
                    if code == "not_owned" || code == "preparation_not_configured" {
                        newly_handled.insert(session.id, session.last_activity_at);
                        if past_max_age {
                            newly_handled_max_age.insert(session.id, session.last_activity_at);
                        }
                    }
                    if code == "process_use_unknown" {
                        report.strip.process_use_unavailable = true;
                        report.strip.more_remaining = true;
                        break;
                    }
                }
            }
        }
        if !newly_refused.is_empty() || !cleared_refusals.is_empty() {
            crate::warn_unrecorded(
                "remember sessions the idle strip refused",
                self.update_meta_json(
                    IDLE_STRIP_REFUSED_KEY,
                    |stored: &mut BTreeMap<i64, (i64, i64)>| {
                        for id in &cleared_refusals {
                            stored.remove(id);
                        }
                        stored.extend(newly_refused.iter().map(|(id, seen)| (*id, *seen)));
                        stored.retain(|_, (_, at)| {
                            now_ms.saturating_sub(*at) < 8 * IDLE_STRIP_REFUSAL_BACKOFF_MS
                        });
                        while stored.len() > IDLE_STRIP_DONE_MAX {
                            stored.pop_first();
                        }
                    },
                ),
            );
        }
        crate::warn_unrecorded(
            "remember the idle strip's backlog",
            self.store_ref()
                .meta_set(IDLE_STRIP_BACKLOG_KEY, &report.idle_backlog.to_string())
                .map_err(|error| error.to_string()),
        );
        self.record_checkout_byte_estimate(&open, &report.samples, now_ms);
        if !newly_handled.is_empty() {
            crate::warn_unrecorded(
                "remember idle-stripped sessions",
                self.update_meta_json(IDLE_STRIP_DONE_KEY, |stored: &mut BTreeMap<i64, i64>| {
                    if memo_outdated {
                        stored.clear();
                    }
                    stored.extend(newly_handled.iter().map(|(id, at)| (*id, *at)));
                    while stored.len() > IDLE_STRIP_DONE_MAX {
                        stored.pop_first();
                    }
                }),
            );
            if !newly_handled_max_age.is_empty() {
                crate::warn_unrecorded(
                    "remember sessions idle-stripped at the artifact max age",
                    self.update_meta_json(
                        IDLE_STRIP_MAX_AGE_DONE_KEY,
                        |stored: &mut BTreeMap<i64, i64>| {
                            if memo_outdated {
                                stored.clear();
                            }
                            stored.extend(newly_handled_max_age.iter().map(|(id, at)| (*id, *at)));
                            while stored.len() > IDLE_STRIP_DONE_MAX {
                                stored.pop_first();
                            }
                        },
                    ),
                );
            }
            if memo_outdated {
                crate::warn_unrecorded(
                    "record the idle strip's coverage rules",
                    self.update_meta_json(IDLE_STRIP_RULES_KEY, |stored: &mut u32| {
                        *stored = IDLE_STRIP_RULES;
                    }),
                );
            }
        }
        crate::phase_telemetry::attach(
            "checkout_idle_strip",
            serde_json::json!({
                "idle_after_hours": idle_hours,
                "artifact_max_age_hours": max_age_ms.map(|age| age / 3_600_000),
                "idle_sessions": idle.len(),
                "backlog": report.idle_backlog,
                "skipped_handled": report.skipped_handled,
                "skipped_backoff": report.skipped_backoff,
                "examined": report.strip.checkouts_examined,
                "checkouts_stripped": report.strip.checkouts_stripped,
                "directories_stripped": report.strip.directories_stripped,
                "noted": report.noted,
                "kept_recent_files": report.kept_recent_files,
                "kept_not_declared_output": report.kept_not_declared,
                "by_kind": report.by_kind,
                "measure_ms": report.measure_ms,
                "memo_outdated": memo_outdated,
                "refused": report.strip.refused,
                "more_remaining": report.strip.more_remaining,
                "phase_ms": {
                    "discover": report.strip.discover_ms,
                    "prove": report.strip.prove_ms,
                    "move": report.strip.move_ms,
                },
            }),
        );
        report
    }

    fn strip_idle_checkout(
        &self,
        session: &Session,
        run: &mut super::retire::RetireRun,
        deadline: Instant,
        venv_markers: &[String],
        receipt_stale_before_ms: Option<i64>,
        report: &mut IdleStripReport,
    ) -> IdleStrip {
        use IdleStrip::Refused;
        // Set exactly when the session is idle for the artifact max age.
        let past_max_age = receipt_stale_before_ms.is_some();
        if Instant::now() >= deadline {
            return Refused("deadline");
        }
        let _git_deadline = crate::git::limit_git_until(deadline);
        let preconditions = crate::phase_telemetry::span("checkout_idle_strip_preconditions");
        let worktree = PathBuf::from(&session.worktree_path);
        match std::fs::symlink_metadata(&worktree) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => return Refused("checkout_missing"),
        }
        let Some(pool) = self.trash_pool_for(session, &worktree) else {
            return Refused("not_owned");
        };
        // Rebuilding must be one command: only a repository that declares
        // its preparation is stripped while a session is still open.
        let Ok(Some(preparation)) = crate::preparation::load_config(&worktree) else {
            return Refused("preparation_not_configured");
        };
        let declared_outputs = preparation
            .steps
            .iter()
            .flat_map(|step| step.outputs.iter().cloned())
            .collect::<Vec<_>>();
        match self.session_operations_unresolved_since_boot(session.id) {
            Ok(false) => {}
            // A session idle for the artifact max age whose only unresolved
            // work is a command's receipt that has not been written for as
            // long never settled. A guarded command that is in fact running
            // is still caught below: by its artifact-use producer marker and
            // lock (`checkout_busy` or `checkout_marker_live`) and by the
            // working-directory and open-file check.
            Ok(true) => match receipt_stale_before_ms {
                Some(stale_before) => {
                    match self.session_operations_unresolved_ignoring_stale_receipts(
                        session.id,
                        stale_before,
                    ) {
                        Ok(false) => crate::phase_telemetry::add(
                            "checkout_idle_strip_stale_receipts_ignored",
                            Duration::ZERO,
                        ),
                        Ok(true) => return Refused("operation_unresolved"),
                        Err(_) => return Refused("operations_unknown"),
                    }
                }
                None => return Refused("operation_unresolved"),
            },
            Err(_) => return Refused("operations_unknown"),
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
        if crate::start_reservation::path_is_reserved(self.main_root(), &worktree) {
            return Refused("start_reserved");
        }
        // Someone working by absolute paths leaves no session activity but
        // does leave modification times.
        let idle_after = Duration::from_millis(
            u64::try_from(crate::auto_cleanup::idle_strip_after_ms(self.main_root())).unwrap_or(0),
        );
        let since = std::time::SystemTime::now()
            .checked_sub(idle_after)
            .unwrap_or(std::time::UNIX_EPOCH);
        let evidence = recent_file_activity(&worktree, since, deadline);
        if evidence == Some(ACTIVITY_UNKNOWN) {
            return Refused("activity_unknown");
        }
        if let Some(evidence) = evidence {
            report.kept_recent_files += 1;
            crate::phase_telemetry::add("checkout_idle_strip_kept_recent_files", Duration::ZERO);
            crate::phase_telemetry::attach(
                "checkout_idle_strip_recent_file_evidence",
                serde_json::json!({ "session_id": session.id, "evidence": evidence }),
            );
            return Refused("recent_file_activity");
        }
        drop(preconditions);
        report.strip.checkouts_listed += 1;
        let discover_started = Instant::now();
        let candidates = super::strip::regenerable_candidates(&worktree, deadline);
        report.strip.discover_ms = report
            .strip
            .discover_ms
            .saturating_add(super::strip::elapsed_ms(discover_started));
        let Ok(candidates) = candidates else {
            return Refused("discovery_failed");
        };
        report.strip.candidates_found += candidates.len();
        let all_candidates = candidates.clone();
        // Only what the session's own preparation recreates: exact outputs
        // and environment roots first; workspace members only together with
        // (or after) their top-level `node_modules`.
        let preparation_window =
            crate::preparation::completed_preparation_window(self.main_root(), session.id);
        let mut primary = Vec::new();
        let mut members = Vec::new();
        let mut not_declared = Vec::new();
        let mut on_demand = Vec::new();
        for candidate in candidates {
            match idle_coverage(&declared_outputs, &candidate) {
                Some(Coverage::Exact) => primary.push(candidate),
                Some(Coverage::EnvironmentRoot) => {
                    crate::phase_telemetry::add(
                        "checkout_idle_strip_covered_environment_root",
                        Duration::ZERO,
                    );
                    primary.push(candidate);
                }
                Some(Coverage::WorkspaceMember) => members.push(candidate),
                // Its step declares files inside the environment but not its
                // interpreter; the recorded preparation still proves it made
                // the environment when `pyvenv.cfg` is dated inside the run.
                Some(Coverage::EnvironmentRootUnproven)
                    if preparation_window.is_some_and(|window| {
                        environment_created_in(&worktree, &candidate.relative, window)
                    }) =>
                {
                    crate::phase_telemetry::add(
                        "checkout_idle_strip_covered_environment_root_recorded",
                        Duration::ZERO,
                    );
                    primary.push(candidate);
                }
                Some(Coverage::EnvironmentRootUnproven) => {
                    *report
                        .strip
                        .refused
                        .entry("environment_root_creation_unproven".to_string())
                        .or_default() += 1;
                    // One directory kept, not a refusal of the checkout.
                    crate::phase_telemetry::add(
                        "checkout_idle_strip_kept_environment_root_creation_unproven",
                        Duration::ZERO,
                    );
                }
                None if rebuilt_on_demand(&candidate, past_max_age) => {
                    crate::phase_telemetry::add(
                        "checkout_idle_strip_covered_cargo_target_max_age",
                        Duration::ZERO,
                    );
                    on_demand.push(candidate.relative.clone());
                    primary.push(candidate);
                }
                None => not_declared.push(candidate),
            }
        }
        let candidates = primary;
        if !not_declared.is_empty() {
            report.kept_not_declared += not_declared.len();
            *report
                .strip
                .refused
                .entry("not_declared_output".to_string())
                .or_default() += not_declared.len();
            for _ in &not_declared {
                crate::phase_telemetry::add(
                    "checkout_idle_strip_kept_not_declared_output",
                    Duration::ZERO,
                );
            }
        }
        if candidates.is_empty() && members.is_empty() {
            tally_kinds(report, &all_candidates, &[], &BTreeMap::new());
            if all_candidates.is_empty() {
                report.samples.insert(session.id, 0);
            }
            return IdleStrip::Done(Vec::new(), Vec::new());
        }
        // A complete open-file observation (shared by the run): any process
        // with its working directory or an open file inside keeps it.
        let snapshot = match run.process_use() {
            Ok(snapshot) => snapshot,
            Err(_) => return Refused("process_use_unknown"),
        };
        if crate::reclaim::open_file_snapshot_blocker_for_worktree(&snapshot, &worktree).is_some() {
            return Refused("process_using_checkout");
        }
        let _claim = match self.try_claim_worktree_lifecycle("retire") {
            Ok(Some(claim)) => claim,
            Ok(None) | Err(_) => return Refused("lifecycle_busy"),
        };
        // Under the claim: still the same open, idle session at the same
        // path, and no start holding the folder.
        match self.store_ref().session(session.id) {
            Ok(current)
                if current.cleanup_state == crate::SessionCleanupState::Open
                    && current.worktree_path == session.worktree_path
                    && current.last_activity_at == session.last_activity_at => {}
            _ => return Refused("changed"),
        }
        if crate::start_reservation::path_is_reserved(self.main_root(), &worktree) {
            return Refused("start_reserved");
        }
        // `submit prepare` holds this lock shared while it writes the
        // directories; busy means a preparation is running. A durable marker
        // left by a producer proved gone (PID/start token and process group
        // absent) does not keep the checkout: such markers leak when a
        // guarded command is killed.
        let _checkout_lock =
            match crate::file_lock::artifact_use::try_artifact_use_reclaimer_past_exited_producers(
                self.main_root(),
                &worktree,
            ) {
                Ok((
                    crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(lock),
                    passed,
                )) => {
                    for _ in 0..passed {
                        crate::phase_telemetry::add(
                            "checkout_idle_strip_exited_producer_marker_passed",
                            Duration::ZERO,
                        );
                    }
                    lock
                }
                Ok((crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(block), _)) => {
                    return Refused(match block {
                        crate::file_lock::artifact_use::ArtifactUseReclaimBlock::UnknownProducer {
                            ..
                        } => "checkout_marker_live",
                        _ => "checkout_busy",
                    });
                }
                Err(_) => return Refused("checkout_lock_unknown"),
            };
        // The run's observation may be minutes old: look again right before
        // the moves unless it is fresh.
        if run
            .process_use_age()
            .is_none_or(|age| age > PROCESS_USE_FRESH_FOR)
        {
            let started = Instant::now();
            let fresh = crate::session_holder::retirement_run_process_use_snapshot(
                deadline.saturating_duration_since(Instant::now()),
            );
            crate::phase_telemetry::add("checkout_idle_strip_fresh_process_use", started.elapsed());
            match fresh {
                crate::reclaim::OpenFileSnapshot::Complete(_) => {
                    if crate::reclaim::open_file_snapshot_blocker_for_worktree(&fresh, &worktree)
                        .is_some()
                    {
                        return Refused("process_using_checkout");
                    }
                }
                crate::reclaim::OpenFileSnapshot::Incomplete { .. } => {
                    return Refused("fresh_process_use_unknown");
                }
            }
        }
        // Bytes of what is being removed or kept by the proofs (covered
        // candidates only), within this checkout's share of the pass budget.
        let mut covered = candidates.clone();
        covered.extend(members.iter().cloned());
        let sizes = self.measure_covered(report, &worktree, &covered, deadline);
        // Every covered candidate is proved before anything moves (the
        // pnpm-member proof reads the top-level `node_modules/.bin`).
        let prove_started = Instant::now();
        let mut verdicts =
            super::strip::prove_strippable_all(&worktree, &covered, deadline, venv_markers);
        report.strip.prove_ms = report
            .strip
            .prove_ms
            .saturating_add(super::strip::elapsed_ms(prove_started));
        let member_verdicts = verdicts.split_off(candidates.len());
        let root_present = std::fs::symlink_metadata(worktree.join("node_modules")).is_ok();
        let mut stripped = self.move_proven(
            StripTarget {
                session_id: session.id,
                worktree: &worktree,
                pool: &pool,
            },
            &candidates,
            verdicts,
            deadline,
            venv_markers,
            &mut report.strip,
        );
        // Workspace members move only once the top-level `node_modules`
        // actually moved (or was already absent), so the declared root
        // install recreates root and members together; otherwise they stay
        // as `member_root_kept`.
        if !members.is_empty() {
            let root_gone =
                !root_present || stripped.iter().any(|relative| relative == "node_modules");
            if root_gone {
                stripped.extend(self.move_proven(
                    StripTarget {
                        session_id: session.id,
                        worktree: &worktree,
                        pool: &pool,
                    },
                    &members,
                    member_verdicts,
                    deadline,
                    venv_markers,
                    &mut report.strip,
                ));
            } else {
                *report
                    .strip
                    .refused
                    .entry("member_root_kept".to_string())
                    .or_default() += members.len();
                for _ in &members {
                    crate::phase_telemetry::add(
                        "checkout_idle_strip_refused_member_root_kept",
                        Duration::ZERO,
                    );
                }
            }
        }
        tally_kinds(report, &all_candidates, &stripped, &sizes);
        // What the checkout still holds, when every covered directory was
        // measured (a partial measurement would understate it).
        if sizes.len() == covered.len() {
            let remaining = covered
                .iter()
                .filter(|candidate| !stripped.contains(&candidate.relative))
                .filter_map(|candidate| sizes.get(&candidate.relative))
                .fold(0_u64, |sum, bytes| sum.saturating_add(*bytes));
            report.samples.insert(session.id, remaining);
        }
        let on_demand = on_demand
            .into_iter()
            .filter(|relative| stripped.contains(relative))
            .collect();
        IdleStrip::Done(stripped, on_demand)
    }
}

impl Broker {
    /// Fold this pass's measurements into the remembered ones and write the
    /// estimate `storage status` shows for this repository's worktree root.
    fn record_checkout_byte_estimate(
        &self,
        open: &[Session],
        measured: &BTreeMap<i64, u64>,
        now_ms: i64,
    ) {
        let _span = crate::phase_telemetry::span("checkout_idle_strip_estimate");
        let mut samples: BTreeMap<i64, u64> = self.meta_json(IDLE_STRIP_SAMPLES_KEY);
        if !measured.is_empty() {
            samples.extend(measured.iter().map(|(id, bytes)| (*id, *bytes)));
            while samples.len() > IDLE_STRIP_SAMPLES_MAX {
                samples.pop_first();
            }
            crate::warn_unrecorded(
                "remember measured checkout bytes",
                self.update_meta_json(IDLE_STRIP_SAMPLES_KEY, |stored: &mut BTreeMap<i64, u64>| {
                    stored.extend(measured.iter().map(|(id, bytes)| (*id, *bytes)));
                    while stored.len() > IDLE_STRIP_SAMPLES_MAX {
                        stored.pop_first();
                    }
                }),
            );
        }
        let ids: Vec<i64> = open.iter().map(|session| session.id).collect();
        let mut estimate = estimate_open_checkout_bytes(&ids, &samples);
        estimate.worktree_root = open
            .first()
            .and_then(|session| Path::new(&session.worktree_path).parent())
            .map(|root| root.to_string_lossy().into_owned())
            .unwrap_or_default();
        estimate.observed_at_ms = now_ms;
        crate::phase_telemetry::attach(
            "checkout_idle_strip_estimate",
            serde_json::json!({
                "open_checkouts": estimate.open_checkouts,
                "sampled": estimate.sampled,
                "measured_bytes": estimate.measured_bytes,
                "estimated_bytes": estimate.estimated_bytes,
            }),
        );
        crate::warn_unrecorded(
            "record the open checkouts' byte estimate",
            self.store_ref().meta_set(
                IDLE_STRIP_ESTIMATE_KEY,
                &serde_json::to_string(&estimate).unwrap_or_default(),
            ),
        );
    }

    /// Bytes of `covered` candidates, within this checkout's budget and what
    /// is left of the pass budget; unmeasured ones are left out.
    fn measure_covered(
        &self,
        report: &mut IdleStripReport,
        worktree: &Path,
        covered: &[super::strip::StripCandidate],
        deadline: Instant,
    ) -> BTreeMap<String, u64> {
        let _span = crate::phase_telemetry::span("checkout_idle_strip_measure");
        let started = Instant::now();
        let pass_left =
            IDLE_STRIP_MEASURE_PASS_BUDGET.saturating_sub(Duration::from_millis(report.measure_ms));
        let measure_deadline = deadline.min(started + IDLE_STRIP_MEASURE_BUDGET.min(pass_left));
        let sizes = covered
            .iter()
            .filter_map(|candidate| {
                if Instant::now() >= measure_deadline {
                    return None;
                }
                crate::disk_headroom::directory_usage_bounded(
                    &worktree.join(&candidate.relative),
                    measure_deadline,
                )
                .map(|usage| (candidate.relative.clone(), usage.bytes))
            })
            .collect();
        report.measure_ms = report
            .measure_ms
            .saturating_add(super::strip::elapsed_ms(started));
        sizes
    }
}

/// Add one checkout's removed and kept directories, by kind, to `report`.
fn tally_kinds(
    report: &mut IdleStripReport,
    candidates: &[super::strip::StripCandidate],
    stripped: &[String],
    sizes: &BTreeMap<String, u64>,
) {
    for candidate in candidates {
        let label = kind_label(candidate);
        let tally = report.by_kind.entry(label).or_default();
        let bytes = sizes.get(&candidate.relative).copied();
        if bytes.is_none() {
            tally.unmeasured += 1;
        }
        if stripped.contains(&candidate.relative) {
            tally.removed += 1;
            tally.removed_bytes = tally.removed_bytes.saturating_add(bytes.unwrap_or(0));
            crate::phase_telemetry::add(removed_counter(label), Duration::ZERO);
        } else {
            tally.kept += 1;
            tally.kept_bytes = tally.kept_bytes.saturating_add(bytes.unwrap_or(0));
            crate::phase_telemetry::add(kept_counter(label), Duration::ZERO);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_refusal_has_its_own_counter() {
        for code in [
            "not_owned",
            "preparation_not_configured",
            "checkout_missing",
            "operation_unresolved",
            "operations_unknown",
            "gate_running",
            "gates_unknown",
            "start_reserved",
            "recent_file_activity",
            "activity_unknown",
            "checkout_busy",
            "checkout_marker_live",
            "checkout_lock_unknown",
            "fresh_process_use_unknown",
            "discovery_failed",
            "process_using_checkout",
            "process_use_unknown",
            "lifecycle_busy",
            "changed",
            "deadline",
        ] {
            assert_eq!(
                idle_refused_counter(code),
                format!("checkout_idle_strip_refused_{code}")
            );
        }
        let note = idle_strip_note(7, &["node_modules".into(), "backend/.venv".into()], 4);
        assert!(note.contains("node_modules, backend/.venv"));
        assert!(note.contains("aethyme broker submit prepare --session 7"));
    }

    /// Sessions the estimate has no measurement for count as the mean of the
    /// measured ones that still hold bytes; stripped ones count as zero and
    /// do not pull that mean down.
    #[test]
    fn the_byte_estimate_extrapolates_from_sessions_still_holding_bytes() {
        let samples = BTreeMap::from([(1, 0_u64), (2, 100), (3, 300), (9, 5_000)]);
        let estimate = estimate_open_checkout_bytes(&[1, 2, 3, 4, 5], &samples);
        assert_eq!(estimate.open_checkouts, 5);
        assert_eq!(estimate.sampled, 3);
        assert_eq!(estimate.measured_bytes, 400);
        // Two unsampled sessions at the mean of 100 and 300.
        assert_eq!(estimate.estimated_bytes, 400 + 2 * 200);
        let nothing = estimate_open_checkout_bytes(&[4, 5], &BTreeMap::new());
        assert_eq!((nothing.sampled, nothing.estimated_bytes), (0, 0));
    }

    /// An environment is proven created by a preparation only when its
    /// `pyvenv.cfg` is dated inside that preparation's run.
    #[test]
    fn an_environment_is_created_by_the_preparation_that_dated_its_config() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("backend/.venv");
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("pyvenv.cfg");
        std::fs::write(&config, "home = /usr/bin\n").unwrap();
        let at = std::time::UNIX_EPOCH + Duration::from_millis(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(&config)
            .unwrap()
            .set_modified(at)
            .unwrap();
        let inside = (990_000, 1_010_000);
        assert!(environment_created_in(dir.path(), "backend/.venv", inside));
        let before = (1_100_000, 1_200_000);
        assert!(!environment_created_in(dir.path(), "backend/.venv", before));
        let after = (100_000, 900_000);
        assert!(!environment_created_in(dir.path(), "backend/.venv", after));
        assert!(!environment_created_in(dir.path(), "other/.venv", inside));
    }

    /// Past the artifact max age, an undeclared Cargo target goes too (Cargo
    /// rebuilds it); nothing else undeclared does, and nothing before.
    #[test]
    fn past_the_max_age_only_cargo_targets_are_rebuilt_on_demand() {
        use super::super::strip::{StripCandidate, StripKind};
        let candidate = |relative: &str, kind| StripCandidate {
            relative: relative.into(),
            kind,
        };
        let target = candidate("target", StripKind::CargoTarget);
        assert!(rebuilt_on_demand(&target, true));
        assert!(!rebuilt_on_demand(&target, false));
        for kind in [
            StripKind::NodeModules,
            StripKind::Venv,
            StripKind::PnpmStore,
        ] {
            assert!(!rebuilt_on_demand(&candidate("x", kind), true), "{kind:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(artifact_max_age_ms(Some(dir.path())), Some(24 * 3_600_000));
        std::fs::write(
            dir.path().join(crate::storage_ledger::BUDGET_FILE),
            "[budget]\nartifact_max_age_hours = 0\n",
        )
        .unwrap();
        assert_eq!(artifact_max_age_ms(Some(dir.path())), None);
        assert_eq!(artifact_max_age_ms(None), None);
    }

    /// Coverage for an open session: exact outputs; a Python environment
    /// root containing a declared output; workspace members while the
    /// top-level `node_modules` is declared. Nothing else.
    #[test]
    fn idle_coverage_takes_exact_outputs_environment_roots_and_members() {
        use super::super::strip::{StripCandidate, StripKind};
        let candidate = |relative: &str, kind| StripCandidate {
            relative: relative.into(),
            kind,
        };
        let outputs = vec![
            "node_modules/".to_string(),
            "tools/.venv/bin/tool".to_string(),
            "tools/.venv/bin/python3".to_string(),
            "cfg/.venv/pyvenv.cfg".to_string(),
            "pip/.venv/bin/ruff".to_string(),
        ];
        assert_eq!(
            idle_coverage(&outputs, &candidate("node_modules", StripKind::NodeModules)),
            Some(Coverage::Exact)
        );
        assert_eq!(
            idle_coverage(&outputs, &candidate("tools/.venv", StripKind::Venv)),
            Some(Coverage::EnvironmentRoot)
        );
        assert_eq!(
            idle_coverage(&outputs, &candidate("cfg/.venv", StripKind::Venv)),
            Some(Coverage::EnvironmentRoot)
        );
        // A step that only installs into an environment it did not create
        // (no interpreter declared) does not prove the environment.
        assert_eq!(
            idle_coverage(&outputs, &candidate("pip/.venv", StripKind::Venv)),
            Some(Coverage::EnvironmentRootUnproven)
        );
        assert_eq!(
            idle_coverage(&outputs, &candidate("other/.venv", StripKind::Venv)),
            None
        );
        assert_eq!(
            idle_coverage(&outputs, &candidate("tools/.venvx", StripKind::Venv)),
            None
        );
        assert_eq!(
            idle_coverage(
                &outputs,
                &candidate("apps/a/node_modules", StripKind::NodeModules)
            ),
            Some(Coverage::WorkspaceMember)
        );
        // Only a Python environment root is taken for an output inside it.
        assert_eq!(
            idle_coverage(
                &["web/node_modules/.bin/x".to_string()],
                &candidate("web/node_modules", StripKind::NodeModules)
            ),
            None
        );
        // Members need the top-level `node_modules` declared.
        assert_eq!(
            idle_coverage(
                &["tools/.venv/bin/python".to_string()],
                &candidate("apps/a/node_modules", StripKind::NodeModules)
            ),
            None
        );
        assert_eq!(
            idle_coverage(&outputs, &candidate("target", StripKind::CargoTarget)),
            None
        );
    }

    /// A declared output covers exactly the directory it names.
    #[test]
    fn a_declared_output_covers_exactly_the_directory_it_names() {
        let outputs = vec![
            "node_modules/".to_string(),
            "./web/build".to_string(),
            "web\\node_modules".to_string(),
        ];
        assert!(declared_output_covers(&outputs, "node_modules"));
        assert!(declared_output_covers(&outputs, "web/node_modules"));
        assert!(!declared_output_covers(&outputs, "web/build/node_modules"));
        assert!(!declared_output_covers(&outputs, ".venv"));
        assert!(!declared_output_covers(&outputs, "target"));
        assert!(!declared_output_covers(&outputs, "node_modules_old"));
        assert!(!declared_output_covers(&outputs, "web"));
        assert!(!declared_output_covers(&[String::new()], "node_modules"));
    }

    fn set_mtime(path: &Path, when: std::time::SystemTime) {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    /// A recent modification of the Git index, the checkout directory or a
    /// tracked top-level directory is activity; old ones are not.
    #[test]
    fn recent_modification_times_count_as_activity() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path();
        let far = || Instant::now() + Duration::from_secs(60);
        let git = |args: &[&str]| {
            assert!(
                crate::git::git_command()
                    .args(args)
                    .current_dir(worktree)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(worktree.join("src/lib.rs"), "x\n").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ]);
        let old = std::time::SystemTime::now() - Duration::from_secs(10 * 3600);
        let since = std::time::SystemTime::now() - Duration::from_secs(4 * 3600);
        let age_all = || {
            for path in [
                worktree.join(".git/index"),
                worktree.join(".git/HEAD"),
                worktree.join("src"),
                worktree.to_path_buf(),
            ] {
                set_mtime(&path, old);
            }
        };
        age_all();
        assert_eq!(recent_file_activity(worktree, since, far()), None);
        set_mtime(&worktree.join(".git/index"), std::time::SystemTime::now());
        assert_eq!(
            recent_file_activity(worktree, since, far()),
            Some("git_index")
        );
        age_all();
        set_mtime(&worktree.join("src"), std::time::SystemTime::now());
        assert_eq!(
            recent_file_activity(worktree, since, far()),
            Some("tracked_directory")
        );
        age_all();
        set_mtime(worktree, std::time::SystemTime::now());
        assert_eq!(
            recent_file_activity(worktree, since, far()),
            Some("checkout_directory")
        );
        // A deep edit: only the file itself is new.
        std::fs::write(worktree.join("src/lib.rs"), "y\n").unwrap();
        age_all();
        assert_eq!(
            recent_file_activity(worktree, since, far()),
            Some("changed_file")
        );
        set_mtime(&worktree.join("src/lib.rs"), old);
        assert_eq!(recent_file_activity(worktree, since, far()), None);
        // `git status` does not finish: the walk decides, proving idle,
        // then seeing a fresh file; out of time it keeps the session.
        crate::worktree_status_activity::STATUS_UNAVAILABLE_FOR_TEST.with(|cell| cell.set(true));
        assert_eq!(recent_file_activity(worktree, since, far()), None);
        std::fs::write(worktree.join("src/lib.rs"), "z\n").unwrap();
        assert_eq!(
            recent_file_activity(worktree, since, far()),
            Some("changed_file_walk")
        );
        set_mtime(&worktree.join("src/lib.rs"), old);
        assert_eq!(
            recent_file_activity(worktree, since, Instant::now()),
            Some(ACTIVITY_UNKNOWN)
        );
        crate::worktree_status_activity::STATUS_UNAVAILABLE_FOR_TEST.with(|cell| cell.set(false));
    }

    /// The notice is created only when absent: a conflict notice is never
    /// overwritten, and a tracked notice path is never written.
    #[test]
    fn the_notice_never_overwrites_another_notice_or_a_tracked_path() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path();
        let git = |args: &[&str]| {
            assert!(
                crate::git::git_command()
                    .args(args)
                    .current_dir(worktree)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::write(worktree.join(".gitignore"), ".aethyme/\n").unwrap();
        let notice = worktree.join(crate::merge::ACTION_REQUIRED_RELPATH);
        let stripped = ["node_modules".to_string()];

        assert_eq!(
            write_idle_strip_notice(worktree, 3, &stripped, 4),
            NoticeOutcome::Written
        );
        assert_eq!(
            write_idle_strip_notice(worktree, 3, &stripped, 4),
            NoticeOutcome::AlreadyPresent
        );
        std::fs::write(&notice, "conflict instructions\n").unwrap();
        assert_eq!(
            write_idle_strip_notice(worktree, 3, &stripped, 4),
            NoticeOutcome::OtherNotice
        );
        assert_eq!(
            std::fs::read_to_string(&notice).unwrap(),
            "conflict instructions\n"
        );
        // Tracked although ignored: never written.
        git(&["add", "-f", crate::merge::ACTION_REQUIRED_RELPATH]);
        std::fs::remove_file(&notice).unwrap();
        assert_eq!(
            write_idle_strip_notice(worktree, 3, &stripped, 4),
            NoticeOutcome::Tracked
        );
        assert!(!notice.exists());
        // Not ignored: not written.
        git(&[
            "rm",
            "-q",
            "--cached",
            crate::merge::ACTION_REQUIRED_RELPATH,
        ]);
        std::fs::write(worktree.join(".gitignore"), "").unwrap();
        assert_eq!(
            write_idle_strip_notice(worktree, 3, &stripped, 4),
            NoticeOutcome::NotIgnored
        );
    }
}
