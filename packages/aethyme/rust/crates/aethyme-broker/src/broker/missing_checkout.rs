//! Finish closed sessions whose checkout is already gone.
//!
//! A closed spawned session normally reaches its terminal `cleaned` state when
//! automatic removal or `finish cleanup` removes its checkout. When the
//! checkout disappeared some other way (a temporary pool root wiped by a
//! restart, a pool emptied when it moved), nothing finishes the row: every
//! removal lap and the priority lane re-examine it (`checkout_missing`) and it
//! inflates "closed sessions still holding a checkout".
//!
//! This lane records such a session as cleaned, exactly as a completed cleanup
//! does (`mark_removed_checkout_sessions_cleaned`, which also settles its
//! cleanup requests), only when the absence is proven on a mounted volume and
//! nothing still needs the checkout. It never deletes anything, and it never
//! deletes the session branch: unlanded commits stay on it.

use super::*;

/// Sessions judged per pass, at most.
const MISSING_CHECKOUT_PAGE: usize = 32;
/// Meta key of the last session id the lane judged.
const MISSING_CHECKOUT_CURSOR_KEY: &str = "missing_checkout.after";
/// Event recorded when a closed session is finished this way.
pub(crate) const SESSION_CHECKOUT_MISSING_FINISHED: &str = "session.checkout_missing_finished";

/// What one pass of the lane did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MissingCheckoutSummary {
    pub examined: usize,
    pub finished: usize,
    /// Sessions kept, per reason code.
    pub kept: std::collections::BTreeMap<&'static str, usize>,
}

/// Why a closed session with a missing checkout cannot be finished yet.
type Keep = &'static str;

impl Broker {
    /// Finish up to one page of closed spawned sessions whose checkout is
    /// proven gone. Bounded by `deadline` and the page size; resumes after the
    /// last session it judged.
    pub(crate) fn finish_closed_sessions_with_missing_checkout(
        &mut self,
        deadline: Option<std::time::Instant>,
    ) -> Result<MissingCheckoutSummary, BrokerOpError> {
        let _span = crate::phase_telemetry::span("missing_checkout_finish");
        let mut summary = MissingCheckoutSummary::default();
        let after = self
            .store_ref()
            .meta_get(MISSING_CHECKOUT_CURSOR_KEY)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let mut page = self
            .store_ref()
            .closed_spawned_sessions_after(after, MISSING_CHECKOUT_PAGE)?;
        if page.is_empty() && after > 0 {
            page = self
                .store_ref()
                .closed_spawned_sessions_after(0, MISSING_CHECKOUT_PAGE)?;
        }
        // One registration listing answers every session of the page.
        let mut registrations: Option<Vec<std::path::PathBuf>> = None;
        for session in page {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                break;
            }
            summary.examined += 1;
            crate::phase_telemetry::add("missing_checkout_examined", std::time::Duration::ZERO);
            let verdict = self.missing_checkout_verdict(&session, &mut registrations, deadline);
            let verdict = match verdict {
                Ok(evidence) => self.finish_missing_checkout_session(&session, evidence, deadline),
                Err(keep) => Err(keep),
            };
            match verdict {
                Ok(()) => {
                    summary.finished += 1;
                    crate::phase_telemetry::add(
                        "missing_checkout_finished",
                        std::time::Duration::ZERO,
                    );
                }
                Err(keep) => {
                    *summary.kept.entry(keep).or_default() += 1;
                }
            }
            crate::warn_unrecorded(
                "advance the missing-checkout lane",
                self.store_ref()
                    .meta_set(MISSING_CHECKOUT_CURSOR_KEY, &session.id.to_string()),
            );
        }
        Ok(summary)
    }

    /// The proof that `session`'s checkout is gone and nothing needs it, as
    /// event evidence, or why not. Cheapest checks first; the only Git work
    /// is one registration listing per page.
    fn missing_checkout_verdict(
        &self,
        session: &Session,
        registrations: &mut Option<Vec<std::path::PathBuf>>,
        deadline: Option<std::time::Instant>,
    ) -> Result<serde_json::Value, Keep> {
        if session.origin != SessionOrigin::Spawned
            || session.cleanup_state != crate::SessionCleanupState::Closed
        {
            return Err("not_closed_spawned");
        }
        // A session closed within its closed-checkout grace is left to the
        // ordinary lanes first, as its checkout would be.
        match crate::load_retention_policy(self.main_root()) {
            Ok(policy) if policy.closed_worktree_in_grace(session, crate::clock::epoch_ms()) => {
                return Err("in_grace");
            }
            Ok(_) => {}
            Err(_) => return Err("policy_unavailable"),
        }
        let worktree = Path::new(&session.worktree_path);
        match std::fs::symlink_metadata(worktree) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err("checkout_present"),
            Err(_) => return Err("checkout_unreadable"),
        }
        // The pool root that held the checkout must be on the disk now, and
        // must be this repository's pool: its root marker lives on the
        // volume, so an unmounted volume or a recreated directory fails here.
        let Some(root) = worktree.parent() else {
            return Err("pool_root_missing");
        };
        #[cfg_attr(not(unix), allow(unused_variables))]
        let root_metadata = match std::fs::symlink_metadata(root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => metadata,
            Ok(_) => return Err("pool_root_unproven"),
            Err(_) => return Err("pool_root_missing"),
        };
        match self.worktree_root_marker_matches_checked(root) {
            Ok(true) => {}
            Ok(false) | Err(_) => return Err("pool_root_unproven"),
        }
        // When the spawn witness recorded the volume, the pool root must be on
        // that same volume today.
        let witness = self
            .store_ref()
            .spawned_worktree_instance_witness(session.id)
            .map_err(|_| "witness_unreadable")?;
        let recorded_volume = witness
            .as_ref()
            .and_then(|witness| witness.volume_uuids.as_ref().map(|uuids| uuids.worktree));
        let volume_evidence = match recorded_volume {
            Some(recorded) => {
                #[cfg(unix)]
                let live = {
                    use std::os::unix::fs::MetadataExt;
                    crate::volume_identity::volume_identity(
                        root,
                        root_metadata.dev(),
                        root_metadata.ino(),
                    )
                    .uuid()
                };
                #[cfg(not(unix))]
                let live: Option<[u8; 16]> = None;
                match live {
                    Some(live) if live == recorded => "volume_uuid_matches",
                    Some(_) => return Err("volume_differs"),
                    None => return Err("volume_unproven"),
                }
            }
            None => "pool_root_marker",
        };
        match self
            .store_ref()
            .other_open_session_at_path(session.id, &session.worktree_path)
        {
            Ok(false) => {}
            Ok(true) => return Err("open_session_at_path"),
            Err(_) => return Err("sessions_unreadable"),
        }
        match self
            .store_ref()
            .latest_session_finish_cleanup_started_event(session.id)
        {
            Ok(None) => {}
            Ok(Some(_)) => return Err("finish_recovery_pending"),
            Err(_) => return Err("finish_recovery_unreadable"),
        }
        let intent_deadline = deadline
            .unwrap_or_else(|| std::time::Instant::now() + std::time::Duration::from_secs(5));
        match crate::archive_cleanup::read_pending_for_session(
            self.main_root(),
            session.id,
            intent_deadline,
        ) {
            Ok(None) => {}
            Ok(Some(_)) => return Err("archive_intent_pending"),
            Err(_) => return Err("archive_intent_unreadable"),
        }
        if registrations.is_none() {
            let _git_deadline = deadline.map(crate::git::limit_git_until);
            *registrations = Some(
                self.repo_handle()
                    .worktree_inventory()
                    .map_err(|_| "registrations_unreadable")?
                    .into_iter()
                    .map(|entry| entry.path)
                    .collect(),
            );
        }
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let canonical_path = worktree
            .file_name()
            .map(|name| canonical_root.join(name))
            .unwrap_or_else(|| worktree.to_path_buf());
        if registrations.as_ref().is_some_and(|paths| {
            paths
                .iter()
                .any(|path| path == worktree || *path == canonical_path)
        }) {
            // Prunable or not, Git still lists it; its own prune settles that
            // registration, and nothing on disk is touched here.
            return Err("git_registered");
        }
        Ok(serde_json::json!({
            "worktree_path": session.worktree_path,
            "pool_root": root.to_string_lossy(),
            "volume_evidence": volume_evidence,
            "git_registered": false,
            "open_session_at_path": false,
            "pending_records": false,
            "deleted": false,
        }))
    }

    /// Record the terminal state under the lifecycle claim, after reading the
    /// session and the proof again.
    fn finish_missing_checkout_session(
        &mut self,
        session: &Session,
        evidence: serde_json::Value,
        deadline: Option<std::time::Instant>,
    ) -> Result<(), Keep> {
        let Ok(Some(_lifecycle_claim)) = self.try_claim_worktree_lifecycle("missing_checkout")
        else {
            return Err("busy");
        };
        let current = self
            .store_ref()
            .session(session.id)
            .map_err(|_| "changed")?;
        if current.cleanup_state != crate::SessionCleanupState::Closed
            || current.worktree_path != session.worktree_path
        {
            return Err("changed");
        }
        let mut registrations = None;
        self.missing_checkout_verdict(&current, &mut registrations, deadline)?;
        self.mark_removed_checkout_sessions_cleaned(&[current.id], current.id)
            .map_err(|_| "record_failed")?;
        crate::warn_unrecorded(
            "record a finished missing checkout",
            self.store().append_event(
                SESSION_CHECKOUT_MISSING_FINISHED,
                Some(current.id),
                Some(&evidence.to_string()),
            ),
        );
        Ok(())
    }
}
