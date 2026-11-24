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
/// One slice stops judging branches after this long. Counted from the start
/// of the judging, not of the listing before it: a slice whose listing alone
/// outlasted it (44 to 753 s in the first 0.8.168 slices of the busiest
/// repository) judged nothing and its cursor did not move.
const SLICE_BUDGET: Duration = Duration::from_secs(20);
/// The lifecycle-lock lane name.
const LIFECYCLE_LANE: &str = "merged_branches";
/// The longest one slice holds the lifecycle claim: deletions it has not
/// reached by then wait for the next slice. A background hold past this is
/// what keeps a foreground `finish` or `start` waiting.
/// It stops starting deletions after 4 s, so the one in progress (a re-judge
/// and a ref delete) still ends under the 5 s that counts as a long hold: at
/// 5 s the busiest repository saw a 5.35 s hold.
const CLAIM_HOLD_BUDGET: Duration = Duration::from_secs(4);
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

#[cfg(test)]
thread_local! {
    /// Test seam: runs after the unclaimed judging, before the slice takes
    /// the lifecycle claim to delete.
    pub(crate) static AFTER_JUDGING: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
thread_local! {
    /// Test seam: a shorter [`SLICE_BUDGET`].
    static SLICE_BUDGET_FOR_TEST: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

fn slice_budget() -> Duration {
    #[cfg(test)]
    if let Some(budget) = SLICE_BUDGET_FOR_TEST.with(std::cell::Cell::get) {
        return budget;
    }
    SLICE_BUDGET
}

fn attach_summary(summary: &MergedBranchSummary) {
    crate::phase_telemetry::attach(
        "merged_branches",
        serde_json::json!({
            "cleaned_session_branches": summary.cleaned_session_branches,
            "deleted": summary.deleted,
            "kept_by_reason": summary.kept_by_reason,
        }),
    );
}

fn record_keep(summary: &mut MergedBranchSummary, reason: &'static str) {
    *summary
        .kept_by_reason
        .entry(reason.to_string())
        .or_default() += 1;
    crate::phase_telemetry::add(keep_counter(reason), Duration::ZERO);
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
    let Ok(Some((_, text))) = crate::cleanup_config::cleanup_config_toml(main_root) else {
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

/// The session id a finished recovery archive's directory name starts with
/// (`<id>-...`); `None` for an unfinished one (`.partial`) and for any other
/// name.
fn archive_owner(name: &str) -> Option<&str> {
    if name.ends_with(".partial") {
        return None;
    }
    name.split_once('-').map(|(owner, _)| owner)
}

/// One slice's listing of the recovery archives, read at the first session
/// that needs it. Used to judge without the lifecycle claim only: a branch it
/// lets go is judged again from a fresh reading before the delete.
#[derive(Default)]
struct SliceArchives(std::cell::OnceCell<Option<BTreeSet<String>>>);

impl SliceArchives {
    /// Whether a finished archive belongs to `owner`; `None` when the
    /// listing failed.
    fn holds(&self, broker: &crate::Broker, owner: &str) -> Option<bool> {
        self.0
            .get_or_init(|| {
                let _span = crate::phase_telemetry::span("merged_branches_archive_listing");
                let mut owners = BTreeSet::new();
                broker.any_archive_owner(|owner| {
                    owners.insert(owner.to_string());
                    false
                })?;
                Some(owners)
            })
            .as_ref()
            .map(|owners| owners.contains(owner))
    }
}

impl crate::Broker {
    /// Whether `wanted` accepts the owner of a finished recovery archive in
    /// either archive directory (the pool-side one and the host state one),
    /// stopping at the first it does. `None` when a directory that exists or
    /// one of its entries cannot be read.
    fn any_archive_owner(&self, mut wanted: impl FnMut(&str) -> bool) -> Option<bool> {
        let roots = {
            let _span = crate::phase_telemetry::span("merged_branches_archive_roots");
            self.recovery_archive_roots()
        };
        for root in roots {
            let listing = match std::fs::read_dir(&root) {
                Ok(listing) => listing,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return None,
            };
            for entry in listing {
                let name = entry.ok()?.file_name();
                crate::phase_telemetry::add("merged_branches_archive_entries", Duration::ZERO);
                if archive_owner(&name.to_string_lossy()).is_some_and(&mut wanted) {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    /// Whether `session` still has state a person could restore, so its
    /// branch must stay: anything not plainly cleaned (open or closed), or a
    /// cleaned session with a finished recovery archive. `None` when that
    /// cannot be told, which also keeps the branch.
    ///
    /// The one place that decides it. Any feature that keeps restorable state
    /// for a session elsewhere (an archive that can bring an idle session back
    /// at its path, say) extends this function, and the branch lane then keeps
    /// such sessions' branches without further change.
    ///
    /// Reads the archive directories now.
    pub(crate) fn has_restorable_state(&self, session: &crate::Session) -> Option<bool> {
        self.has_restorable_state_with(session, |owner| {
            let _span = crate::phase_telemetry::span("merged_branches_archive_listing_fresh");
            self.any_archive_owner(|found| found == owner)
        })
    }

    /// The rule itself, for every caller of [`Self::has_restorable_state`]
    /// and for the judging loop, which differ only in where the archive names
    /// come from: `archived` says whether a finished recovery archive belongs
    /// to a session id, `None` when that cannot be told, and is asked only
    /// when nothing cheaper decides.
    fn has_restorable_state_with(
        &self,
        session: &crate::Session,
        archived: impl FnOnce(&str) -> Option<bool>,
    ) -> Option<bool> {
        if session.cleanup_state != crate::SessionCleanupState::Cleaned {
            return Some(true);
        }
        // Retired through an archive that `start --reuse` restores at the
        // same path: recorded with the session, and marked by a tombstone.
        let recorded = {
            let _span = crate::phase_telemetry::span("merged_branches_archive_record");
            self.session_restorable_archive(session.id).is_some()
        };
        if recorded {
            return Some(true);
        }
        let tombstone = {
            let _span = crate::phase_telemetry::span("merged_branches_tombstone");
            crate::broker::is_checkout_tombstone(Path::new(&session.worktree_path))
        };
        if tombstone {
            return Some(true);
        }
        archived(&session.id.to_string())
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
        let repo = self.repo_handle();
        let Some(default) = crate::session_push::tracked_default(repo) else {
            return Err("no_fetched_default_branch");
        };
        let default_branch = default.branch.clone();
        let listing = {
            let _span = crate::phase_telemetry::span("merged_branches_list_branches");
            repo.for_each_local_branch()
                .map_err(|_| "branch_listing_failed")?
        };
        let checked_out = {
            let _span = crate::phase_telemetry::span("merged_branches_list_checkouts");
            repo.worktree_inventory()
                .map_err(|_| "worktree_listing_failed")?
                .into_iter()
                .filter_map(|entry| entry.branch)
                .filter_map(|branch| branch.strip_prefix("refs/heads/").map(str::to_string))
                .collect::<BTreeSet<_>>()
        };
        let mut sessions_by_branch = {
            let _span = crate::phase_telemetry::span("merged_branches_list_sessions");
            let names = listing
                .iter()
                .map(|(name, _)| name.as_str())
                .filter(|name| *name != default_branch)
                .collect();
            self.store_ref()
                .sessions_on_branches(&names)
                .map_err(|_| "session_lookup_failed")?
        };

        // Local branches every one of whose sessions is cleaned, by name.
        // One with a session not cleaned is kept, and counted, without taking
        // a place in the window: on the busiest repository those were 170 of
        // the 200 a slice judged.
        let mut candidates = Vec::new();
        for (name, tip) in &listing {
            if *name == default_branch {
                continue;
            }
            let Some(sessions) = sessions_by_branch.remove(name) else {
                continue;
            };
            if sessions
                .iter()
                .any(|session| session.cleanup_state != crate::SessionCleanupState::Cleaned)
            {
                record_keep(summary, "session_not_cleaned");
                continue;
            }
            candidates.push((name.clone(), tip.clone(), sessions));
        }
        summary.cleaned_session_branches = candidates.len() as u64;
        if candidates.is_empty() {
            attach_summary(summary);
            return Ok(());
        }
        after_listing();
        let cursor = self.store_ref().meta_get(CURSOR_KEY).ok().flatten();
        let start = cursor
            .as_deref()
            .and_then(|cursor| {
                candidates
                    .iter()
                    .position(|(name, _, _)| name.as_str() > cursor)
            })
            .unwrap_or(0);
        let window: Vec<_> = candidates
            .iter()
            .cycle()
            .skip(start)
            .take(CHECKS_PER_RUN.min(candidates.len()))
            .collect();
        // Judge from facts read without the lifecycle claim: the ancestry of
        // two fixed commits (Git), the recovery archives (one listing of
        // their directories for the slice), the sessions and checkouts
        // listed above. Holding the claim for that kept a foreground
        // `finish` or `start` waiting for the whole slice budget (7 to 26 s
        // per run on the busiest repository). A branch judged for deletion is judged again under
        // the claim, just before the delete, which itself refuses a tip that
        // moved; nothing judged kept is touched.
        let archives = SliceArchives::default();
        let mut judged = 0usize;
        let mut to_delete = Vec::new();
        {
            let _span = crate::phase_telemetry::span("merged_branches_judge");
            let started = Instant::now();
            for (index, (name, tip, sessions)) in window.iter().enumerate() {
                if started.elapsed() >= slice_budget() {
                    crate::phase_telemetry::add("merged_branches_slice_budget", Duration::ZERO);
                    break;
                }
                judged = index + 1;
                let checked_out = checked_out.contains(name);
                // A fact is read only when no earlier one keeps the branch.
                let recovery_archive = !checked_out && {
                    let _span = crate::phase_telemetry::span("merged_branches_restorable_state");
                    sessions.iter().any(|session| {
                        self.has_restorable_state_with(session, |owner| archives.holds(self, owner))
                            != Some(false)
                    })
                };
                let merged = (!checked_out && !recovery_archive)
                    .then(|| {
                        let _span = crate::phase_telemetry::span("merged_branches_ancestry");
                        repo.is_ancestor_checked(tip, &default.commit).ok()
                    })
                    .flatten();
                let facts = BranchFacts {
                    session_not_cleaned: false,
                    checked_out,
                    recovery_archive,
                    merged,
                };
                match judge(facts) {
                    Verdict::Delete => to_delete.push(index),
                    Verdict::Keep(reason) => record_keep(summary, reason),
                }
            }
        }
        // The lane's last processed branch: everything up to it was judged
        // and, when judged for deletion, deleted or kept under the claim.
        let mut processed = judged;
        let mut deleted = 0usize;
        if !to_delete.is_empty() {
            #[cfg(test)]
            AFTER_JUDGING.with(|hook| {
                if let Some(hook) = hook.borrow_mut().as_mut() {
                    hook();
                }
            });
            let Some(_lock) = crate::file_lock::try_acquire_repository_lifecycle_lock(
                self.main_root(),
                LIFECYCLE_LANE,
            )
            .ok()
            .flatten() else {
                return Err("yielded_to_foreground");
            };
            let _span = crate::phase_telemetry::span("merged_branches_claimed");
            let claimed = Instant::now();
            for index in to_delete {
                if deleted >= DELETES_PER_RUN || claimed.elapsed() >= CLAIM_HOLD_BUDGET {
                    crate::phase_telemetry::add(
                        if deleted >= DELETES_PER_RUN {
                            "merged_branches_slice_budget"
                        } else {
                            "merged_branches_claim_budget"
                        },
                        Duration::ZERO,
                    );
                    // Resume from this branch next run.
                    processed = index;
                    break;
                }
                let (name, tip, _) = window[index];
                // The facts above were read before the lifecycle claim; a
                // checkout started or reopened meanwhile could use the
                // branch. Under the claim, judge it again just before the
                // delete, which itself refuses a tip that moved.
                let verdict = match self.keep_reason_now(name) {
                    Some(reason) => Verdict::Keep(reason),
                    None => match repo.delete_branch_ref_checked(name, tip) {
                        Ok(()) => Verdict::Delete,
                        Err(_) => Verdict::Keep("delete_failed"),
                    },
                };
                match verdict {
                    Verdict::Delete => {
                        deleted += 1;
                        crate::phase_telemetry::add("merged_branches_deleted", Duration::ZERO);
                    }
                    Verdict::Keep(reason) => record_keep(summary, reason),
                }
            }
        }
        let last = match processed.checked_sub(1) {
            Some(index) => window[index].0.clone(),
            None => cursor.clone().unwrap_or_default(),
        };
        summary.deleted = deleted as u64;
        summary.deleted_total = summary.deleted_total.saturating_add(deleted as u64);
        crate::warn_unrecorded(
            "advance the merged-branch cursor",
            self.store_ref().meta_set(CURSOR_KEY, &last),
        );
        attach_summary(summary);
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
