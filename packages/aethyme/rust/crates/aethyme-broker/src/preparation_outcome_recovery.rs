//! Keeping a finished dependency install when only its bookkeeping failed,
//! and waiting for another session's install instead of failing.
//!
//! On a pnpm repository whose worktrees sit on an external volume and whose
//! host state sits on the internal disk, 38 of 51 preparations in one
//! five-hour window failed (2026-11-26). Installs that had run for up to
//! 30 minutes were discarded because:
//!
//! * the running step's heartbeat applied the 8 GiB build reserve to the host
//!   resource database, a few KB of bookkeeping, and stopped the install when
//!   the internal disk dipped to the reserve;
//! * a heartbeat that missed the 30 s lease TTL (an I/O-starved process) found
//!   its lease quarantined and stopped the install, although a quarantined
//!   lease whose holder runs still excludes every other holder;
//! * every uncertain outcome then re-ran every step from the start;
//! * a second session arriving during a 30-minute install failed after 60 s
//!   and its agent polled `prepare status` in a loop.
//!
//! The decisions here are pure, so each is tested without a broker.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::cache_activity::{PreparationCacheActivity, STALE_AFTER_MS};
use super::{PreparationRecord, PreparationState};

/// Free bytes a running step's heartbeat requires on the broker's state
/// (`.aethyme`, the host resource database): enough to write the step's
/// outcome (a record of a few KB, a SQLite page or two), never the 8 GiB
/// build reserve. That reserve keeps protecting builds: it is checked before
/// each step is dispatched, never after.
pub(super) const BOOKKEEPING_RESERVE_BYTES: u64 = 256 * 1024 * 1024;

/// Free inodes a running step's heartbeat requires on the broker's state.
pub(super) const BOOKKEEPING_RESERVE_INODES: u64 = 1_000;

/// Free bytes a running step's heartbeat requires on the volumes the install
/// writes (the worktree, the shared cache directory): the step is stopped
/// before it drives its own volume to zero.
pub(super) const INSTALL_RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Free inodes a running step's heartbeat requires on the install's volumes.
pub(super) const INSTALL_RESERVE_INODES: u64 = 10_000;

/// The fallback journal's name in the session's scratch directory, on the
/// worktree's volume: where a finished preparation's record goes when the
/// repository's own record cannot be written.
pub(super) const FALLBACK_OUTCOME_FILE: &str = "preparation-outcome.json";

/// The longest a waiting preparation follows another session's progressing
/// fill of the shared cache before it reports the conflict.
pub(super) const SHARED_CACHE_FOLLOW_MAX: Duration = Duration::from_secs(2 * 60 * 60);

/// When an agent should try again after the shared cache stayed busy: soon
/// when the holder reported no live progress (it may be gone), later when it
/// is still working.
pub(super) const RETRY_AFTER_HOLDER_SILENT_SECONDS: u64 = 30;
pub(super) const RETRY_AFTER_HOLDER_WORKING_SECONDS: u64 = 120;

pub(super) fn fallback_outcome_path(scratch: &Path) -> PathBuf {
    scratch.join(FALLBACK_OUTCOME_FILE)
}

/// Whether a failed renewal of the lease a running step holds must stop the
/// step.
///
/// The host quarantines an active lease whose expiry passed. While its holder
/// process runs, a quarantined lease keeps its allocations and keeps
/// excluding every other holder (only a dead holder's lease is reclaimed), so
/// the step's exclusion still holds and its release still works. Stopping the
/// install would discard the work and protect nothing. Every other failure
/// (ownership, a missing lease, the database) still stops it.
pub(super) fn renewal_failure_stops_step(error: &crate::HostResourceError) -> bool {
    !matches!(
        error,
        crate::HostResourceError::RequestNotActive { state, .. } if state == "quarantined"
    )
}

/// Which record describes a session's last preparation when both the
/// repository's record and a fallback journal exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReconciledRecord {
    Primary,
    Fallback,
}

/// The fallback journal wins when the repository's record is missing or
/// unreadable, when it is from a later run, or when it records the end of
/// the same run the repository's record still calls in progress.
pub(super) fn reconcile_choice(
    primary: Option<&PreparationRecord>,
    fallback: &PreparationRecord,
) -> ReconciledRecord {
    let Some(primary) = primary else {
        return ReconciledRecord::Fallback;
    };
    if primary.session_id != fallback.session_id {
        return ReconciledRecord::Primary;
    }
    let later_run = fallback.started_at > primary.started_at;
    let same_run_ended = fallback.started_at == primary.started_at
        && fallback.digest == primary.digest
        && primary.state == PreparationState::InProgress
        && fallback.state != PreparationState::InProgress;
    if later_run || same_run_ended {
        ReconciledRecord::Fallback
    } else {
        ReconciledRecord::Primary
    }
}

/// What a preparation does about a prior run whose outcome was never
/// recorded (`interrupted`: the record says in progress, its preparer is
/// gone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UncertainVerdict {
    /// Not an interrupted run, or nothing of it can be trusted: run every
    /// step.
    RunAll,
    /// Every step finished, verified its outputs and was recorded before
    /// the run was interrupted: accept the install without running anything.
    AcceptComplete,
    /// The leading steps named here finished and were recorded: run only
    /// the rest.
    Resume(BTreeSet<String>),
}

/// Judge an interrupted run against the checkout as it is now.
///
/// A step counts only when the run recorded it as finished (exit 0, outputs
/// verified) before it was interrupted, its inputs still hash to the same
/// key, and no declared output is missing or was stripped since. A step whose
/// own exit was never observed is run again even when its outputs exist: a
/// package manager stopped mid-import leaves `node_modules` present and
/// incomplete, and accepting that would hand an agent a broken checkout.
/// Only a leading run of finished steps is skipped, so a later step never
/// runs on an earlier one that did not finish.
pub(super) fn uncertain_outcome_verdict(
    state: PreparationState,
    record: Option<&PreparationRecord>,
    source_digest: &str,
    missing_outputs: &[String],
    stripped: bool,
    steps: &[&str],
) -> UncertainVerdict {
    if state != PreparationState::Interrupted {
        return UncertainVerdict::RunAll;
    }
    let Some(record) = record else {
        return UncertainVerdict::RunAll;
    };
    if record.source_digest != source_digest || !missing_outputs.is_empty() || stripped {
        return UncertainVerdict::RunAll;
    }
    let finished: BTreeSet<&str> = record
        .steps
        .iter()
        .filter(|timing| timing.outcome == "ok")
        .map(|timing| timing.name.as_str())
        .collect();
    let completed: BTreeSet<String> = steps
        .iter()
        .take_while(|step| finished.contains(**step))
        .map(|step| (*step).to_owned())
        .collect();
    if completed.is_empty() {
        UncertainVerdict::RunAll
    } else if completed.len() == steps.len() {
        UncertainVerdict::AcceptComplete
    } else {
        UncertainVerdict::Resume(completed)
    }
}

/// What a preparation whose `--wait` ran out on a busy shared cache does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FollowDecision {
    /// The holder is another session making progress: keep waiting for it,
    /// printing this line once per wait window.
    Follow { line: String },
    /// Report the conflict.
    GiveUp,
}

/// Keep waiting while the holder of the shared cache reports live progress.
///
/// `--wait 0s` still fails fast, a holder with no fresh progress record (an
/// older broker, a cache sweep, a stalled or dead process) is not followed,
/// and following stops after [`SHARED_CACHE_FOLLOW_MAX`].
pub(super) fn follow_holder_decision(
    wait: Duration,
    followed_for: Duration,
    holder: Option<&PreparationCacheActivity>,
    now_ms: i64,
) -> FollowDecision {
    if wait.is_zero() || followed_for >= SHARED_CACHE_FOLLOW_MAX {
        return FollowDecision::GiveUp;
    }
    let Some(holder) = holder else {
        return FollowDecision::GiveUp;
    };
    let silent_ms = now_ms.saturating_sub(holder.updated_at_ms);
    if silent_ms > STALE_AFTER_MS {
        return FollowDecision::GiveUp;
    }
    let since = holder
        .lease_acquired_at_ms
        .unwrap_or(holder.operation_started_at_ms);
    FollowDecision::Follow {
        line: format!(
            "waiting for session {}'s {} (running {}s, last progress {}s ago); this prepare \
             goes on when it finishes and reuses the filled cache; `--wait 0s` fails fast instead",
            holder.session_id,
            holder.operation.as_str(),
            now_ms.saturating_sub(since).max(0) / 1_000,
            silent_ms.max(0) / 1_000,
        ),
    }
}

/// The structured next step an agent gets when the shared cache stayed busy,
/// so it schedules one retry instead of polling `prepare status`.
pub(super) fn busy_cache_retry_action(
    session_id: i64,
    holder: Option<&PreparationCacheActivity>,
) -> serde_json::Value {
    let (retry_after, holder_session) = match holder {
        Some(holder) => (RETRY_AFTER_HOLDER_WORKING_SECONDS, Some(holder.session_id)),
        None => (RETRY_AFTER_HOLDER_SILENT_SECONDS, None),
    };
    serde_json::json!({
        "kind": "retry_prepare",
        "reason": "shared_cache_busy",
        "retry_after_seconds": retry_after,
        "holder_session_id": holder_session,
        "command": super::prepare_command(session_id),
    })
}

/// The structured next step after a step whose outcome could not be
/// confirmed: retry now; steps the run recorded as finished are verified,
/// not run again.
pub(super) fn uncertain_outcome_retry_action(session_id: i64) -> serde_json::Value {
    serde_json::json!({
        "kind": "retry_prepare",
        "reason": "outcome_unconfirmed",
        "retry_after_seconds": 0,
        "command": super::prepare_command(session_id),
        "note": "steps the interrupted run recorded as finished are verified against their outputs and not run again",
    })
}

/// How long a `prepare status` reader should wait before reading again, or
/// `None` when nothing is running and the answer will not change by itself.
pub(super) fn status_retry_after_seconds(
    state: PreparationState,
    waiting_on_another_session: bool,
) -> Option<u64> {
    if waiting_on_another_session {
        Some(RETRY_AFTER_HOLDER_WORKING_SECONDS)
    } else if state == PreparationState::InProgress {
        Some(RETRY_AFTER_HOLDER_SILENT_SECONDS)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::PreparationStepTiming;
    use super::*;
    use crate::preparation::cache_activity::PreparationCacheOperation;

    fn record(
        state: PreparationState,
        started_at: i64,
        steps: &[(&str, &str)],
    ) -> PreparationRecord {
        PreparationRecord {
            schema_version: super::super::PREPARATION_SCHEMA_VERSION,
            session_id: 7,
            state,
            digest: "digest".into(),
            source_digest: "source".into(),
            started_at,
            completed_at: None,
            failed_step: None,
            exit_code: None,
            host_lease_id: Some("lease".into()),
            host_lease_generation: Some(1),
            package_store: None,
            runtimes: Vec::new(),
            steps: steps
                .iter()
                .enumerate()
                .map(|(index, (name, outcome))| PreparationStepTiming {
                    index,
                    name: (*name).into(),
                    outcome: (*outcome).into(),
                    ..PreparationStepTiming::default()
                })
                .collect(),
        }
    }

    fn holder(updated_at_ms: i64) -> PreparationCacheActivity {
        serde_json::from_value(serde_json::json!({
            "schema": "aethyme.preparation-cache-activity/v1",
            "repository": "repo",
            "session_id": 42,
            "holder_pid": 1,
            "operation": "preparation_step",
            "items_done": 0,
            "items_total_is_lower_bound": false,
            "lease_acquired_at_ms": 1_000,
            "operation_started_at_ms": 1_000,
            "updated_at_ms": updated_at_ms,
        }))
        .expect("activity record")
    }

    /// 8.0 GiB free on the host state disk refused the build reserve and
    /// stopped a 21-minute install; a running step only needs room for its
    /// bookkeeping.
    #[test]
    fn a_running_step_needs_room_for_bookkeeping_not_for_a_build() {
        let paths = [PathBuf::from("host-state/host-resources.db")];
        let just_under_build_reserve = |_: &Path| {
            Some(crate::disk_headroom::DiskHeadroom {
                bytes: crate::DEFAULT_GATE_HEADROOM_BYTES - 1,
                inodes: Some(1_000_000),
            })
        };
        assert!(
            super::super::preparation_storage_with(&paths, |_| None, just_under_build_reserve)
                .is_err(),
            "dispatching a step still needs the build reserve"
        );
        assert!(
            super::super::preparation_storage_within(
                &paths,
                |_| None,
                just_under_build_reserve,
                BOOKKEEPING_RESERVE_BYTES,
                BOOKKEEPING_RESERVE_INODES,
            )
            .is_ok()
        );
        let nearly_full = |_: &Path| {
            Some(crate::disk_headroom::DiskHeadroom {
                bytes: BOOKKEEPING_RESERVE_BYTES - 1,
                inodes: Some(1_000_000),
            })
        };
        assert!(
            super::super::preparation_storage_within(
                &paths,
                |_| None,
                nearly_full,
                BOOKKEEPING_RESERVE_BYTES,
                BOOKKEEPING_RESERVE_INODES,
            )
            .is_err(),
            "a disk too full to record anything still stops the step"
        );
    }

    /// A running step keeps 2 GiB on the volumes it installs into and only
    /// 256 MiB on the broker's state.
    #[test]
    fn a_running_step_keeps_an_install_floor_and_a_bookkeeping_floor() {
        let worktree = PathBuf::from("worktree");
        let host_db = PathBuf::from("host-state/host-resources.db");
        let free = |worktree_bytes: u64, db_bytes: u64| {
            move |path: &Path| {
                Some(crate::disk_headroom::DiskHeadroom {
                    bytes: if path == Path::new("worktree") {
                        worktree_bytes
                    } else {
                        db_bytes
                    },
                    inodes: Some(1_000_000),
                })
            }
        };
        let check = |worktree_bytes, db_bytes| {
            super::super::running_step_storage_with(
                std::slice::from_ref(&worktree),
                std::slice::from_ref(&host_db),
                |_| None,
                free(worktree_bytes, db_bytes),
            )
        };
        let gib = 1024 * 1024 * 1024;
        assert!(check(3 * gib, BOOKKEEPING_RESERVE_BYTES).is_ok());
        assert!(
            check(INSTALL_RESERVE_BYTES - 1, 3 * gib).is_err(),
            "an install may not drive its own volume below 2 GiB"
        );
        assert!(
            check(3 * gib, gib).is_ok(),
            "1 GiB on the host state disk is plenty for bookkeeping"
        );
        assert!(
            check(3 * gib, BOOKKEEPING_RESERVE_BYTES - 1).is_err(),
            "a host state disk too full to record anything stops the step"
        );
        let few_inodes = |_: &Path| {
            Some(crate::disk_headroom::DiskHeadroom {
                bytes: 3 * gib,
                inodes: Some(INSTALL_RESERVE_INODES - 1),
            })
        };
        assert!(
            super::super::running_step_storage_with(
                std::slice::from_ref(&worktree),
                &[],
                |_| None,
                few_inodes,
            )
            .is_err()
        );
        assert!(
            super::super::running_step_storage_with(
                &[],
                std::slice::from_ref(&host_db),
                |_| None,
                few_inodes,
            )
            .is_ok()
        );
    }

    #[test]
    fn only_a_quarantined_lease_lets_the_running_step_continue() {
        let quarantined = crate::HostResourceError::RequestNotActive {
            request_id: "lease".into(),
            state: "quarantined".into(),
        };
        assert!(!renewal_failure_stops_step(&quarantined));
        let released = crate::HostResourceError::RequestNotActive {
            request_id: "lease".into(),
            state: "released".into(),
        };
        assert!(renewal_failure_stops_step(&released));
        assert!(renewal_failure_stops_step(
            &crate::HostResourceError::LeaseNotFound("lease".into())
        ));
        assert!(renewal_failure_stops_step(
            &crate::HostResourceError::OwnershipMismatch("lease".into())
        ));
    }

    #[test]
    fn the_fallback_journal_wins_only_with_newer_or_final_evidence() {
        let fallback = record(PreparationState::Current, 10, &[("deps", "ok")]);
        assert_eq!(
            reconcile_choice(None, &fallback),
            ReconciledRecord::Fallback
        );
        let in_progress = record(PreparationState::InProgress, 10, &[]);
        assert_eq!(
            reconcile_choice(Some(&in_progress), &fallback),
            ReconciledRecord::Fallback,
            "the same run's end beats its in-progress record"
        );
        let older = record(PreparationState::Current, 5, &[]);
        assert_eq!(
            reconcile_choice(Some(&older), &fallback),
            ReconciledRecord::Fallback
        );
        let newer = record(PreparationState::InProgress, 20, &[]);
        assert_eq!(
            reconcile_choice(Some(&newer), &fallback),
            ReconciledRecord::Primary,
            "a later run's record is never replaced by an earlier outcome"
        );
        let same_final = record(PreparationState::Failed, 10, &[]);
        assert_eq!(
            reconcile_choice(Some(&same_final), &fallback),
            ReconciledRecord::Primary
        );
    }

    #[test]
    fn an_interrupted_run_whose_every_step_finished_is_accepted() {
        let prior = record(
            PreparationState::InProgress,
            1,
            &[("deps", "ok"), ("build", "ok")],
        );
        assert_eq!(
            uncertain_outcome_verdict(
                PreparationState::Interrupted,
                Some(&prior),
                "source",
                &[],
                false,
                &["deps", "build"],
            ),
            UncertainVerdict::AcceptComplete
        );
    }

    #[test]
    fn an_interrupted_run_resumes_after_its_last_finished_step() {
        let prior = record(
            PreparationState::InProgress,
            1,
            &[("deps", "ok"), ("build", "skipped"), ("docs", "ok")],
        );
        assert_eq!(
            uncertain_outcome_verdict(
                PreparationState::Interrupted,
                Some(&prior),
                "source",
                &[],
                false,
                &["deps", "build", "docs"],
            ),
            UncertainVerdict::Resume(BTreeSet::from(["deps".to_owned()])),
            "only a leading run of finished steps is skipped"
        );
    }

    #[test]
    fn an_unobserved_step_changed_inputs_or_missing_outputs_run_everything() {
        let unobserved = record(PreparationState::InProgress, 1, &[("deps", "skipped")]);
        let finished = record(PreparationState::InProgress, 1, &[("deps", "ok")]);
        let verdict =
            |state, prior: &PreparationRecord, source: &str, missing: &[String], stripped| {
                uncertain_outcome_verdict(state, Some(prior), source, missing, stripped, &["deps"])
            };
        assert_eq!(
            verdict(
                PreparationState::Interrupted,
                &unobserved,
                "source",
                &[],
                false
            ),
            UncertainVerdict::RunAll,
            "outputs that exist are not proof the step finished"
        );
        assert_eq!(
            verdict(
                PreparationState::Interrupted,
                &finished,
                "changed",
                &[],
                false
            ),
            UncertainVerdict::RunAll
        );
        assert_eq!(
            verdict(
                PreparationState::Interrupted,
                &finished,
                "source",
                &["node_modules".to_owned()],
                false
            ),
            UncertainVerdict::RunAll
        );
        assert_eq!(
            verdict(
                PreparationState::Interrupted,
                &finished,
                "source",
                &[],
                true
            ),
            UncertainVerdict::RunAll
        );
        assert_eq!(
            verdict(
                PreparationState::InProgress,
                &finished,
                "source",
                &[],
                false
            ),
            UncertainVerdict::RunAll,
            "a run whose preparer may still be alive is never judged"
        );
        assert_eq!(
            uncertain_outcome_verdict(
                PreparationState::Interrupted,
                None,
                "source",
                &[],
                false,
                &["deps"]
            ),
            UncertainVerdict::RunAll
        );
    }

    #[test]
    fn a_progressing_holder_is_followed_and_a_silent_one_is_not() {
        let now = 61_000;
        match follow_holder_decision(
            Duration::from_secs(60),
            Duration::ZERO,
            Some(&holder(now - 5_000)),
            now,
        ) {
            FollowDecision::Follow { line } => {
                assert!(line.contains("session 42"), "{line}");
                assert!(line.contains("running 60s"), "{line}");
                assert!(line.contains("last progress 5s ago"), "{line}");
            }
            FollowDecision::GiveUp => panic!("a progressing holder is followed"),
        }
        assert_eq!(
            follow_holder_decision(
                Duration::from_secs(60),
                Duration::ZERO,
                Some(&holder(now - STALE_AFTER_MS - 1)),
                now
            ),
            FollowDecision::GiveUp
        );
        assert_eq!(
            follow_holder_decision(Duration::from_secs(60), Duration::ZERO, None, now),
            FollowDecision::GiveUp
        );
        assert_eq!(
            follow_holder_decision(Duration::ZERO, Duration::ZERO, Some(&holder(now)), now),
            FollowDecision::GiveUp,
            "--wait 0s still fails fast"
        );
        assert_eq!(
            follow_holder_decision(
                Duration::from_secs(60),
                SHARED_CACHE_FOLLOW_MAX,
                Some(&holder(now)),
                now
            ),
            FollowDecision::GiveUp
        );
        assert_eq!(
            holder(now).operation,
            PreparationCacheOperation::PreparationStep
        );
    }

    #[test]
    fn retry_actions_and_status_tell_agents_when_to_come_back() {
        let busy = busy_cache_retry_action(9, Some(&holder(0)));
        assert_eq!(
            busy["retry_after_seconds"],
            RETRY_AFTER_HOLDER_WORKING_SECONDS
        );
        assert_eq!(busy["holder_session_id"], 42);
        assert_eq!(busy["command"], "aethyme broker submit prepare --session 9");
        let silent = busy_cache_retry_action(9, None);
        assert_eq!(
            silent["retry_after_seconds"],
            RETRY_AFTER_HOLDER_SILENT_SECONDS
        );
        assert!(silent["holder_session_id"].is_null());
        assert_eq!(uncertain_outcome_retry_action(9)["retry_after_seconds"], 0);

        assert_eq!(
            status_retry_after_seconds(PreparationState::Required, true),
            Some(RETRY_AFTER_HOLDER_WORKING_SECONDS)
        );
        assert_eq!(
            status_retry_after_seconds(PreparationState::InProgress, false),
            Some(RETRY_AFTER_HOLDER_SILENT_SECONDS)
        );
        assert_eq!(
            status_retry_after_seconds(PreparationState::Current, false),
            None
        );
        assert_eq!(
            status_retry_after_seconds(PreparationState::Failed, false),
            None
        );
    }
}
