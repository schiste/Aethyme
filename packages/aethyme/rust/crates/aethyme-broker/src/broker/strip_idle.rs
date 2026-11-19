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
pub(crate) const IDLE_STRIP_RULES: u32 = 2;
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
const IDLE_STRIP_MEASURE_BUDGET: Duration = Duration::from_secs(2);
/// The longest all byte measurements of one pass may take together.
const IDLE_STRIP_MEASURE_PASS_BUDGET: Duration = Duration::from_secs(6);

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
    /// These relative paths moved into the trash (none: nothing to strip).
    Done(Vec<String>),
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
) -> Option<&'static str> {
    let newer = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified >= since)
    };
    if let Some(admin) = git_admin_dir(worktree) {
        if newer(&admin.join("index")) {
            return Some("git_index");
        }
        if newer(&admin.join("HEAD")) {
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
        None => Some(ACTIVITY_UNKNOWN),
    }
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
        let mut idle = match self.store_ref().live_sessions() {
            Ok(sessions) => sessions
                .into_iter()
                .filter(|session| {
                    session.cleanup_state == crate::SessionCleanupState::Open
                        && session.origin == crate::SessionOrigin::Spawned
                        && now_ms.saturating_sub(session.last_activity_at) >= idle_after_ms
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                crate::warn_unrecorded::<(), _>("list idle open sessions", Err(error));
                return report;
            }
        };
        idle.sort_by_key(|session| (session.last_activity_at, session.id));
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
        let mut newly_handled = BTreeMap::new();
        let mut listed = 0_usize;
        for session in &idle {
            if handled.get(&session.id) == Some(&session.last_activity_at) {
                report.skipped_handled += 1;
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
                &mut report,
            ) {
                IdleStrip::Done(stripped) => {
                    if transient_before == super::strip::transient_refusals(&report.strip) {
                        newly_handled.insert(session.id, session.last_activity_at);
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
                    if code == "not_owned" || code == "preparation_not_configured" {
                        newly_handled.insert(session.id, session.last_activity_at);
                    }
                    if code == "process_use_unknown" {
                        report.strip.process_use_unavailable = true;
                        report.strip.more_remaining = true;
                        break;
                    }
                }
            }
        }
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
                "idle_sessions": idle.len(),
                "backlog": report.idle_backlog,
                "skipped_handled": report.skipped_handled,
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
        report: &mut IdleStripReport,
    ) -> IdleStrip {
        use IdleStrip::Refused;
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
            Ok(true) => return Refused("operation_unresolved"),
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
        let evidence = recent_file_activity(&worktree, since);
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
        let mut primary = Vec::new();
        let mut members = Vec::new();
        let mut not_declared = Vec::new();
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
            return IdleStrip::Done(Vec::new());
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
        // directories; busy means a preparation is running.
        let _checkout_lock = match crate::file_lock::artifact_use::try_artifact_use_reclaimer(
            self.main_root(),
            &worktree,
        ) {
            Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(lock)) => lock,
            Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(_)) => {
                return Refused("checkout_busy");
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
        IdleStrip::Done(stripped)
    }
}

impl Broker {
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
        assert_eq!(recent_file_activity(worktree, since), None);
        set_mtime(&worktree.join(".git/index"), std::time::SystemTime::now());
        assert_eq!(recent_file_activity(worktree, since), Some("git_index"));
        age_all();
        set_mtime(&worktree.join("src"), std::time::SystemTime::now());
        assert_eq!(
            recent_file_activity(worktree, since),
            Some("tracked_directory")
        );
        age_all();
        set_mtime(worktree, std::time::SystemTime::now());
        assert_eq!(
            recent_file_activity(worktree, since),
            Some("checkout_directory")
        );
        // A deep edit: only the file itself is new.
        std::fs::write(worktree.join("src/lib.rs"), "y\n").unwrap();
        age_all();
        assert_eq!(recent_file_activity(worktree, since), Some("changed_file"));
        set_mtime(&worktree.join("src/lib.rs"), old);
        assert_eq!(recent_file_activity(worktree, since), None);
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
