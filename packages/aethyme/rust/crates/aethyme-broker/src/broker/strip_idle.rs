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
//! - no start holding the folder's reservation.
//!
//! A live holder alone does not block: the directories are regenerable and
//! the open-file check sees whatever still uses them. Under the lifecycle
//! claim, right before anything moves, the session must still be open at the
//! same path with the same last activity, and the reservation is asked
//! again.
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
        "discovery_failed" => "checkout_idle_strip_refused_discovery_failed",
        "process_using_checkout" => "checkout_idle_strip_refused_process_using_checkout",
        "process_use_unknown" => "checkout_idle_strip_refused_process_use_unknown",
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

/// Leave the idle strip's notice at `.aethyme/broker-action-required.md` in
/// `worktree`, the file agents are told to read first, unless another
/// notice is already there (a conflict notice wins) or Git would see the
/// file (the checkout's ignore rules must cover it: the notice never shows
/// up in `git status`). Returns whether it was written.
fn write_idle_strip_notice(
    worktree: &Path,
    session_id: i64,
    stripped: &[String],
    idle_hours: i64,
) -> bool {
    let path = worktree.join(crate::merge::ACTION_REQUIRED_RELPATH);
    if path.exists() && !crate::preparation::is_idle_strip_notice(&path) {
        crate::phase_telemetry::add("checkout_idle_strip_notice_skipped_other", Duration::ZERO);
        return false;
    }
    let mut command = crate::git::git_command();
    command
        .args(["check-ignore", "-q", "--no-index", "--"])
        .arg(crate::merge::ACTION_REQUIRED_RELPATH)
        .current_dir(worktree);
    let ignored = crate::bounded_output::output_limited(
        &mut command,
        crate::git::git_timeout(),
        4096,
    )
    .is_ok_and(|output| {
        matches!(output, crate::bounded_output::LimitedOutput::Completed(output) if output.status.success())
    });
    if !ignored {
        crate::phase_telemetry::add(
            "checkout_idle_strip_notice_skipped_not_ignored",
            Duration::ZERO,
        );
        return false;
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
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, body));
    written.is_ok()
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
                    if write_idle_strip_notice(
                        Path::new(&session.worktree_path),
                        session.id,
                        &stripped,
                        idle_hours,
                    ) {
                        crate::phase_telemetry::add(
                            "checkout_idle_strip_notice_written",
                            Duration::ZERO,
                        );
                    }
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
        if !matches!(crate::preparation::load_config(&worktree), Ok(Some(_))) {
            return Refused("preparation_not_configured");
        }
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
}
