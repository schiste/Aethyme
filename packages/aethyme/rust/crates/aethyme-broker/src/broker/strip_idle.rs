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
//! Only directories a step of the session's own preparation declares as an
//! output (or that lie inside one) are stripped from an open session:
//! `submit prepare` rebuilds exactly those, so the one-command promise
//! holds. Every other catalogued directory is kept and counted as
//! `not_declared_output`. (The closed-checkout strip is unchanged.)
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
        "{}\n# Dependencies were removed from this checkout\n\n\
         Session {session_id} had no activity for {idle_hours} hour(s) and no process in this \
         checkout, so its regenerable dependency directories were moved out for deletion:\n\n{}\n\n\
         Your work is untouched: commits, staged and unstaged changes and every other file are \
         where you left them.\n\n\
         Rebuild the dependencies with one command before building or testing:\n\n    {}\n\n\
         This notice disappears once the session is prepared again.\n",
        crate::preparation::IDLE_STRIP_NOTICE_HEADER,
        stripped
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
/// the checkout): it names that directory or one of its ancestors.
pub(crate) fn declared_output_covers(outputs: &[String], relative: &str) -> bool {
    outputs.iter().any(|output| {
        let output = output.replace('\\', "/");
        let output = output.trim_start_matches("./").trim_end_matches('/');
        !output.is_empty()
            && (relative == output
                || relative
                    .strip_prefix(output)
                    .is_some_and(|rest| rest.starts_with('/')))
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
    tracked_top_level_directories(worktree)
        .iter()
        .any(|name| newer(&worktree.join(name)))
        .then_some("tracked_directory")
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
        let handled: BTreeMap<i64, i64> = self.meta_json(IDLE_STRIP_DONE_KEY);
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
                    stored.extend(newly_handled.iter().map(|(id, at)| (*id, *at)));
                    while stored.len() > IDLE_STRIP_DONE_MAX {
                        stored.pop_first();
                    }
                }),
            );
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
        if let Some(evidence) = recent_file_activity(&worktree, since) {
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
        // Only what the session's own preparation rebuilds.
        let (candidates, not_declared): (Vec<_>, Vec<_>) = candidates
            .into_iter()
            .partition(|candidate| declared_output_covers(&declared_outputs, &candidate.relative));
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
        if candidates.is_empty() {
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
        IdleStrip::Done(self.prove_and_move(
            StripTarget {
                session_id: session.id,
                worktree: &worktree,
                pool: &pool,
            },
            &candidates,
            deadline,
            venv_markers,
            &mut report.strip,
        ))
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

    /// Only directories a declared step output names, or lies above, are
    /// stripped from an open session.
    #[test]
    fn a_declared_output_covers_itself_and_what_lies_inside() {
        let outputs = vec!["node_modules/".to_string(), "./web/build".to_string()];
        assert!(declared_output_covers(&outputs, "node_modules"));
        assert!(declared_output_covers(&outputs, "web/build/node_modules"));
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
