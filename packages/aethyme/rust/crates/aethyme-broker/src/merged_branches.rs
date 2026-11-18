//! Delete the local branches of cleaned sessions once their work is merged.
//!
//! Cleaning a session removes its checkout but some paths keep its local
//! branch. On the busiest repository 1,230 local session branches had piled
//! up, 811 of them fully merged into the default branch, and every Git
//! command that lists refs paid for them.
//!
//! A branch is deleted only when every condition holds: every session that
//! ever used it is cleaned; its tip is an ancestor of the remote default
//! branch as last fetched; no worktree has it checked out; no recovery
//! archive of its sessions still needs it to recreate a checkout; and the
//! ref still names the tip that was judged (compare-and-swap delete).
//! Anything unmerged, unknown or unreadable is kept and counted. Local refs
//! only: no remote branch is deleted, nothing is fetched or pruned.
//!
//! The detached worker runs this lane: a bounded slice per run, from a
//! cursor, under a nonblocking lifecycle claim that yields to foreground
//! commands, so the backlog drains over several runs and newly cleaned
//! sessions are picked up on the next one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

/// Broker meta key: the last branch name a slice judged.
const CURSOR_KEY: &str = "merged_branches.cursor";
/// Broker meta key: the last slice's summary ([`MergedBranchSummary`]).
pub(crate) const SUMMARY_KEY: &str = "merged_branches.summary";
/// Branches one slice judges at most.
const CHECKS_PER_RUN: usize = 200;
/// Branches one slice deletes at most.
const DELETES_PER_RUN: usize = 100;
/// One slice stops starting work after this long.
const SLICE_BUDGET: Duration = Duration::from_secs(20);
/// The lifecycle-lock lane name.
const LIFECYCLE_LANE: &str = "merged_branches";
/// Status advice id.
pub(crate) const ADVICE_ID: &str = "cleanup.merged-session-branches";

/// What the last slice saw and did, as recorded for `status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MergedBranchSummary {
    pub(crate) at: i64,
    /// Local branches whose sessions are all cleaned, before this slice.
    pub(crate) cleaned_session_branches: u64,
    pub(crate) deleted: u64,
    #[serde(default)]
    pub(crate) deleted_total: u64,
    #[serde(default)]
    pub(crate) kept_by_reason: BTreeMap<String, u64>,
    /// Why the slice did not run, when it did not.
    pub(crate) skipped: Option<String>,
}

/// Why one branch was kept, or that it may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Delete,
    Keep(&'static str),
}

/// The facts one branch is judged on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BranchFacts {
    /// Some session that used the branch is not cleaned.
    pub(crate) session_not_cleaned: bool,
    pub(crate) checked_out: bool,
    pub(crate) recovery_archive: bool,
    /// `Some(true)` merged into the fetched default branch, `Some(false)`
    /// not, `None` unknown.
    pub(crate) merged: Option<bool>,
}

/// The deletion rule, as a pure function.
pub(crate) fn judge(facts: BranchFacts) -> Verdict {
    if facts.session_not_cleaned {
        return Verdict::Keep("session_not_cleaned");
    }
    if facts.checked_out {
        return Verdict::Keep("checked_out");
    }
    if facts.recovery_archive {
        return Verdict::Keep("recovery_archive");
    }
    match facts.merged {
        Some(true) => Verdict::Delete,
        Some(false) => Verdict::Keep("unmerged"),
        None => Verdict::Keep("unknown"),
    }
}

/// `[cleanup] delete_merged_session_branches` (default true).
pub(crate) fn enabled(main_root: &Path) -> bool {
    let Ok(Some((_, text))) =
        aethyme_config::read_config_toml(&main_root.join(aethyme_config::CONFIG_DIR), "config")
    else {
        return true;
    };
    toml::from_str::<toml::Value>(&text)
        .ok()
        .and_then(|root| {
            root.get("cleanup")
                .and_then(|cleanup| cleanup.get("delete_merged_session_branches"))
                .and_then(toml::Value::as_bool)
        })
        .unwrap_or(true)
}

fn keep_counter(reason: &str) -> &'static str {
    match reason {
        "session_not_cleaned" => "merged_branches_kept_session_not_cleaned",
        "checked_out" => "merged_branches_kept_checked_out",
        "recovery_archive" => "merged_branches_kept_recovery_archive",
        "unmerged" => "merged_branches_kept_unmerged",
        "unknown" => "merged_branches_kept_unknown",
        "delete_failed" => "merged_branches_kept_delete_failed",
        "start_reserved" => "merged_branches_kept_start_reserved",
        _ => "merged_branches_kept_other",
    }
}

/// Whether a checkout has `refs/heads/<name>` checked out right now, read
/// from Git's own registrations: the primary checkout's `HEAD` and every
/// linked worktree's `HEAD` under the common Git directory, including a
/// branch a rebase or bisect in progress will return to. `None` when they cannot be
/// read, which keeps the branch.
fn branch_checked_out_now(repo: &crate::GitRepo, name: &str) -> Option<bool> {
    let common = repo.git_common_dir().ok()?;
    let wanted = format!("refs/heads/{name}");
    let names_branch = |path: &Path| -> Option<bool> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let text = text.trim();
                Some(
                    text.strip_prefix("ref:").map(str::trim) == Some(wanted.as_str())
                        || text == wanted,
                )
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        }
    };
    let admin_dirs = std::iter::once(Ok(common.clone())).chain(
        match std::fs::read_dir(common.join("worktrees")) {
            Ok(entries) => entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Vec<_>>(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return None,
        },
    );
    for admin in admin_dirs {
        let admin = admin.ok()?;
        for file in ["HEAD", "rebase-merge/head-name", "rebase-apply/head-name"] {
            if names_branch(&admin.join(file))? {
                return Some(true);
            }
        }
        // A bisect returns to the branch it started from, which it records
        // by short name (or as a commit when started detached).
        match std::fs::read_to_string(admin.join("BISECT_START")) {
            Ok(text) if text.trim() == name || text.trim() == wanted => return Some(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some(false)
}

impl crate::Broker {
    /// Whether `session` still has state a person could restore, so its
    /// branch must stay: anything not plainly cleaned (open or closed), or a
    /// cleaned session with a finished recovery archive. `None` when that
    /// cannot be told, which also keeps the branch.
    ///
    /// The one place that decides it. Any feature that keeps restorable state
    /// for a session elsewhere (an archive that can bring an idle session back
    /// at its path, say) extends this function, and the branch lane then keeps
    /// such sessions' branches without further change.
    pub(crate) fn has_restorable_state(&self, session: &crate::Session) -> Option<bool> {
        if session.cleanup_state != crate::SessionCleanupState::Cleaned {
            return Some(true);
        }
        let root = self.recovery_archive_root().ok()?;
        let listing = match std::fs::read_dir(&root) {
            Ok(listing) => listing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(false),
            Err(_) => return None,
        };
        let prefix = format!("{}-", session.id);
        let mut found = false;
        for entry in listing {
            let name = entry.ok()?.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&prefix) && !name.ends_with(".partial") {
                found = true;
                break;
            }
        }
        Some(found)
    }

    /// Why the branch must stay, judged afresh: every session that uses it
    /// now, and whether a checkout has it checked out now. `None` when it may
    /// go.
    fn keep_reason_now(&self, name: &str) -> Option<&'static str> {
        let Ok(sessions) = self.store_ref().sessions_on_branch(name) else {
            return Some("unknown");
        };
        if sessions.is_empty() {
            return Some("unknown");
        }
        if sessions
            .iter()
            .any(|session| session.cleanup_state != crate::SessionCleanupState::Cleaned)
        {
            return Some("session_not_cleaned");
        }
        // A start or restore registering a checkout for one of these
        // sessions holds a start reservation and adds its worktree outside
        // the lifecycle claim; its branch must survive the add.
        if sessions.iter().any(|session| {
            crate::start_reservation::path_is_reserved(
                self.main_root(),
                Path::new(&session.worktree_path),
            )
        }) {
            return Some("start_reserved");
        }
        for session in &sessions {
            match self.has_restorable_state(session) {
                Some(false) => {}
                Some(true) => return Some("recovery_archive"),
                None => return Some("unknown"),
            }
        }
        match branch_checked_out_now(self.repo_handle(), name) {
            Some(false) => None,
            Some(true) => Some("checked_out"),
            None => Some("unknown"),
        }
    }

    /// One bounded slice of the merged-branch lane. Never fetches.
    pub(crate) fn run_merged_branch_lane(&self) -> MergedBranchSummary {
        self.run_merged_branch_lane_with(&mut || {})
    }

    /// [`Self::run_merged_branch_lane`] with a hook that runs after the
    /// unlocked listing and before the lifecycle claim (tests).
    pub(crate) fn run_merged_branch_lane_with(
        &self,
        after_listing: &mut dyn FnMut(),
    ) -> MergedBranchSummary {
        let _span = crate::phase_telemetry::span("merged_branches_slice");
        let now = crate::clock::epoch_ms();
        let previous = self
            .store_ref()
            .meta_get(SUMMARY_KEY)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str::<MergedBranchSummary>(&text).ok())
            .unwrap_or_default();
        let mut summary = MergedBranchSummary {
            at: now,
            deleted_total: previous.deleted_total,
            ..MergedBranchSummary::default()
        };
        let skipped = |summary: &mut MergedBranchSummary, why: &str| {
            crate::phase_telemetry::add("merged_branches_skipped", Duration::ZERO);
            summary.skipped = Some(why.to_string());
        };
        if !enabled(self.main_root()) {
            skipped(&mut summary, "disabled");
        } else {
            match self.merged_branch_slice(&mut summary, after_listing) {
                Ok(()) => {}
                Err(why) => skipped(&mut summary, why),
            }
        }
        crate::warn_unrecorded(
            "record the merged-branch summary",
            serde_json::to_string(&summary)
                .map_err(|error| error.to_string())
                .and_then(|text| {
                    self.store_ref()
                        .meta_set(SUMMARY_KEY, &text)
                        .map_err(|error| error.to_string())
                }),
        );
        summary
    }

    fn merged_branch_slice(
        &self,
        summary: &mut MergedBranchSummary,
        after_listing: &mut dyn FnMut(),
    ) -> Result<(), &'static str> {
        let started = Instant::now();
        let repo = self.repo_handle();
        let Some(default) = crate::session_push::tracked_default(repo) else {
            return Err("no_fetched_default_branch");
        };
        let default_branch = default.branch.clone();
        let listing = repo
            .for_each_local_branch()
            .map_err(|_| "branch_listing_failed")?;
        let checked_out = repo
            .worktree_inventory()
            .map_err(|_| "worktree_listing_failed")?
            .into_iter()
            .filter_map(|entry| entry.branch)
            .filter_map(|branch| branch.strip_prefix("refs/heads/").map(str::to_string))
            .collect::<BTreeSet<_>>();

        // Local branches every one of whose sessions is cleaned, by name.
        let mut candidates = Vec::new();
        for (name, tip) in &listing {
            if *name == default_branch {
                continue;
            }
            let sessions = self
                .store_ref()
                .sessions_on_branch(name)
                .map_err(|_| "session_lookup_failed")?;
            if sessions.is_empty() {
                continue;
            }
            candidates.push((name.clone(), tip.clone(), sessions));
        }
        summary.cleaned_session_branches = candidates
            .iter()
            .filter(|(_, _, sessions)| {
                sessions
                    .iter()
                    .all(|session| session.cleanup_state == crate::SessionCleanupState::Cleaned)
            })
            .count() as u64;
        if candidates.is_empty() {
            return Ok(());
        }
        after_listing();
        let Some(_lock) = crate::file_lock::try_acquire_repository_lifecycle_lock(
            self.main_root(),
            LIFECYCLE_LANE,
        )
        .ok()
        .flatten() else {
            return Err("yielded_to_foreground");
        };
        let cursor = self.store_ref().meta_get(CURSOR_KEY).ok().flatten();
        let start = cursor
            .as_deref()
            .and_then(|cursor| {
                candidates
                    .iter()
                    .position(|(name, _, _)| name.as_str() > cursor)
            })
            .unwrap_or(0);
        let mut last = cursor.clone().unwrap_or_default();
        let mut deleted = 0usize;
        for (name, tip, sessions) in candidates
            .iter()
            .cycle()
            .skip(start)
            .take(CHECKS_PER_RUN.min(candidates.len()))
        {
            if started.elapsed() >= SLICE_BUDGET || deleted >= DELETES_PER_RUN {
                crate::phase_telemetry::add("merged_branches_slice_budget", Duration::ZERO);
                break;
            }
            last.clone_from(name);
            let facts = BranchFacts {
                session_not_cleaned: sessions
                    .iter()
                    .any(|session| session.cleanup_state != crate::SessionCleanupState::Cleaned),
                checked_out: checked_out.contains(name),
                recovery_archive: sessions
                    .iter()
                    .any(|session| self.has_restorable_state(session) != Some(false)),
                merged: repo.is_ancestor_checked(tip, &default.commit).ok(),
            };
            // The facts above were read before the lifecycle claim; a
            // checkout started or reopened meanwhile could use the branch.
            // Under the claim, judge it again just before the delete, which
            // itself refuses a tip that moved.
            let verdict = match judge(facts) {
                Verdict::Delete => match self.keep_reason_now(name) {
                    Some(reason) => Verdict::Keep(reason),
                    None => match repo.delete_branch_ref_checked(name, tip) {
                        Ok(()) => Verdict::Delete,
                        Err(_) => Verdict::Keep("delete_failed"),
                    },
                },
                keep => keep,
            };
            match verdict {
                Verdict::Delete => {
                    deleted += 1;
                    crate::phase_telemetry::add("merged_branches_deleted", Duration::ZERO);
                }
                Verdict::Keep(reason) => {
                    *summary
                        .kept_by_reason
                        .entry(reason.to_string())
                        .or_default() += 1;
                    crate::phase_telemetry::add(keep_counter(reason), Duration::ZERO);
                }
            }
        }
        summary.deleted = deleted as u64;
        summary.deleted_total = summary.deleted_total.saturating_add(deleted as u64);
        crate::warn_unrecorded(
            "advance the merged-branch cursor",
            self.store_ref().meta_set(CURSOR_KEY, &last),
        );
        crate::phase_telemetry::attach(
            "merged_branches",
            serde_json::json!({
                "cleaned_session_branches": summary.cleaned_session_branches,
                "deleted": summary.deleted,
                "kept_by_reason": summary.kept_by_reason,
            }),
        );
        Ok(())
    }
}

/// `cleanup.merged-session-branches`: local branches of cleaned sessions
/// remain. Reads the last slice's summary only.
pub(crate) fn status_advice(store: &crate::BrokerStore) -> Option<crate::StatusAdvice> {
    let summary = store
        .meta_get(SUMMARY_KEY)
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str::<MergedBranchSummary>(&text).ok())?;
    if summary.cleaned_session_branches == 0 {
        return None;
    }
    let kept = summary
        .kept_by_reason
        .iter()
        .map(|(reason, count)| format!("{reason} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut evidence = vec![
        format!(
            "local branches of cleaned sessions: {}",
            summary.cleaned_session_branches
        ),
        format!(
            "deleted by the last slice (at {}): {}; in all: {}",
            summary.at, summary.deleted, summary.deleted_total
        ),
    ];
    if !kept.is_empty() {
        evidence.push(format!("kept by the last slice: {kept}"));
    }
    if let Some(why) = &summary.skipped {
        evidence.push(format!("last slice skipped: {why}"));
    }
    Some(crate::StatusAdvice {
        id: ADVICE_ID,
        severity: crate::StatusAdviceSeverity::Notice,
        reason: "local branches of cleaned sessions slow every Git command that lists refs; merged ones are deleted a bounded slice at a time by the background worker",
        summary: format!(
            "{} local branch(es) of cleaned sessions remain; the background worker deletes the merged ones ({} so far)",
            summary.cleaned_session_branches, summary.deleted_total
        ),
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands: vec![
            "set [cleanup] delete_merged_session_branches = false in .aethyme/config.toml to stop it".into(),
        ],
    })
}

#[cfg(test)]
mod tests;
