//! The rule that lets the abandonment window close a session before the
//! window of its work class: nothing on the session can be lost.
//!
//! A session closed by the abandonment window keeps its branch, and a
//! checkout that holds uncommitted work is archived before it is retired.
//! That makes closing safe at the class windows (`[cleanup]
//! stale_*_close_after_hours`), which give an agent time to come back. A
//! shorter close, down to `session_abandoned_after_hours`, is allowed only
//! when there is nothing to come back for. Evaluated now, never remembered:
//!
//! - no in-progress merge, rebase, cherry-pick, revert or bisect;
//! - no stash made on the session branch;
//! - every tip (HEAD and the branch) is contained in the default branch, in
//!   the session branch as the remote holds it now (`ls-remote`, never the
//!   local tracking ref, which survives a remote deletion), or in the head
//!   of a merged pull request; the diff base gets no exemption;
//! - no staged change, no unstaged change, no untracked file Git does not
//!   ignore, and no index entry flagged assume-unchanged or skip-worktree
//!   (status cannot see changes to those; such a checkout is not examined).
//!
//! "No exemption for the diff base" is this rule's: a tip equal to the diff
//! base must itself be contained like any other. The work-class computation
//! (`stale_ref_class`, which only picks the class window) is different: it
//! drops a tip equal to the diff base when that base is contained in the
//! upstream default branch or in the primary checkout's HEAD, so a session
//! without commits in a repository with no remote still reads `landed`.
//!
//! The checks run cheapest first and stop at the first that fails. Anything
//! that cannot be read is "not examined", which never allows a close.

use std::time::Duration;

/// Why a session may not close before its class window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnsafeReason {
    /// A merge, rebase, cherry-pick, revert or bisect is in progress.
    InProgress,
    /// A stash was made on the session branch.
    Stash,
    /// HEAD holds a commit the default branch, the remote session branch
    /// and every merged pull request lack.
    HeadNotContained,
    /// The session branch holds such a commit.
    BranchNotContained,
    /// The index differs from HEAD.
    Staged,
    /// The working tree differs from the index.
    Unstaged,
    /// An untracked file Git does not ignore.
    Untracked,
    /// HEAD of an existing checkout could not be read.
    Unreadable,
    /// A check could not be completed (Git failed, the remote could not be
    /// asked).
    NotExamined,
}

impl UnsafeReason {
    #[cfg(test)]
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Stash => "stash",
            Self::HeadNotContained => "head_not_contained",
            Self::BranchNotContained => "branch_not_contained",
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Untracked => "untracked",
            Self::Unreadable => "unreadable",
            Self::NotExamined => "not_examined",
        }
    }

    /// The counter one keep for this reason adds.
    pub(crate) const fn telemetry_name(self) -> &'static str {
        match self {
            Self::InProgress => "session_abandonment_kept_unsafe_in_progress",
            Self::Stash => "session_abandonment_kept_unsafe_stash",
            Self::HeadNotContained => "session_abandonment_kept_unsafe_head_not_contained",
            Self::BranchNotContained => "session_abandonment_kept_unsafe_branch_not_contained",
            Self::Staged => "session_abandonment_kept_unsafe_staged",
            Self::Unstaged => "session_abandonment_kept_unsafe_unstaged",
            Self::Untracked => "session_abandonment_kept_unsafe_untracked",
            Self::Unreadable => "session_abandonment_kept_unsafe_unreadable",
            Self::NotExamined => "session_abandonment_kept_unsafe_not_examined",
        }
    }
}

/// One check of the rule, so a test can neuter it and show which fixture
/// depends on it.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SafetyCheck {
    InProgress,
    Stash,
    HeadContained,
    BranchContained,
    /// The remote is asked for the session branch, not the local tracking ref.
    RemoteAsked,
    /// The diff base is not exempt.
    DiffBaseNotExempt,
    Staged,
    Unstaged,
    /// Assume-unchanged and skip-worktree entries make the tree unexamined.
    IndexFlags,
    Untracked,
}

#[cfg(test)]
thread_local! {
    static NEUTERED: std::cell::Cell<Option<SafetyCheck>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_LOOKS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The next `looks` rule evaluations on this thread fail as `Untracked`, as
/// a transient file would make them (tests only).
#[cfg(test)]
pub(crate) fn fail_next_looks_for_test(looks: u32) {
    FAIL_NEXT_LOOKS.with(|remaining| remaining.set(looks));
}

/// Skip `check` on this thread until `None` (tests only).
#[cfg(test)]
pub(crate) fn neuter_for_test(check: Option<SafetyCheck>) {
    NEUTERED.with(|neutered| neutered.set(check));
}

#[cfg(test)]
fn neutered(check: SafetyCheck) -> bool {
    NEUTERED.with(|neutered| neutered.get() == Some(check))
}

#[cfg(not(test))]
#[allow(dead_code)]
#[derive(Clone, Copy)]
enum SafetyCheck {
    InProgress,
    Stash,
    HeadContained,
    BranchContained,
    RemoteAsked,
    DiffBaseNotExempt,
    Staged,
    Unstaged,
    /// Assume-unchanged and skip-worktree entries make the tree unexamined.
    IndexFlags,
    Untracked,
}

#[cfg(not(test))]
const fn neutered(_check: SafetyCheck) -> bool {
    false
}

impl crate::Broker {
    /// Whether `session` may close before its class window: `Ok` only when
    /// every check of the rule holds now (see the module documentation).
    pub(crate) fn abandonment_safety_check(
        &self,
        session: &crate::Session,
    ) -> Result<(), UnsafeReason> {
        let _span = crate::phase_telemetry::span("abandonment_safety_check");
        #[cfg(test)]
        let forced_failure = FAIL_NEXT_LOOKS.with(|remaining| {
            let left = remaining.get();
            remaining.set(left.saturating_sub(1));
            left > 0
        });
        #[cfg(not(test))]
        let forced_failure = false;
        let verdict = if forced_failure {
            Err(UnsafeReason::Untracked)
        } else {
            self.abandonment_safety_verdict(session)
        };
        crate::phase_telemetry::add(
            match verdict {
                Ok(()) => "abandonment_safety_safe",
                Err(_) => "abandonment_safety_unsafe",
            },
            Duration::ZERO,
        );
        verdict
    }

    fn abandonment_safety_verdict(&self, session: &crate::Session) -> Result<(), UnsafeReason> {
        let repo = self.repo_handle();
        let worktree = std::path::Path::new(&session.worktree_path);
        let checkout_exists = std::fs::symlink_metadata(worktree).is_ok();
        let checkout = if checkout_exists {
            Some(crate::git::GitRepo::discover(worktree).map_err(|_| UnsafeReason::Unreadable)?)
        } else {
            None
        };

        // Cheapest first: marker files, then one stash listing.
        if let Some(checkout) = &checkout
            && !neutered(SafetyCheck::InProgress)
        {
            let git_dir = crate::git::checked_worktree_git_dir(checkout.root())
                .ok()
                .flatten()
                .ok_or(UnsafeReason::NotExamined)?;
            for marker in [
                "MERGE_HEAD",
                "CHERRY_PICK_HEAD",
                "REVERT_HEAD",
                "BISECT_LOG",
                "rebase-merge",
                "rebase-apply",
            ] {
                match std::fs::symlink_metadata(git_dir.join(marker)) {
                    Ok(_) => return Err(UnsafeReason::InProgress),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(UnsafeReason::NotExamined),
                }
            }
        }
        if !neutered(SafetyCheck::Stash) {
            let stashes = repo
                .stash_subjects()
                .map_err(|_| UnsafeReason::NotExamined)?;
            let made_here = [
                format!("WIP on {}:", session.branch),
                format!("On {}:", session.branch),
            ];
            if stashes
                .iter()
                .any(|subject| made_here.iter().any(|prefix| subject.starts_with(prefix)))
            {
                return Err(UnsafeReason::Stash);
            }
        }

        // Every tip contained somewhere it survives a close.
        let head = match &checkout {
            Some(checkout) => Some(
                checkout
                    .head_commit()
                    .map_err(|_| UnsafeReason::Unreadable)?,
            ),
            None => None,
        };
        let branch_tip = repo.resolve_ref(&format!("refs/heads/{}", session.branch));
        let upstream = repo.upstream_default();
        let merged_heads =
            if crate::session_auto_close::PullRequestMerges::read(self.store_ref(), session.id)
                .is_some_and(|merges| merges.all_merged())
            {
                self.store_ref()
                    .merged_pull_request_heads(session.id)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
        let mut remote_branch: Option<Option<String>> = None;
        let contains = |ancestor: &str, descendant: &str| {
            ancestor == descendant
                || matches!(repo.is_ancestor_checked(ancestor, descendant), Ok(true))
        };
        let mut contained = |tip: &str| -> Result<bool, UnsafeReason> {
            if neutered(SafetyCheck::DiffBaseNotExempt) && session.diff_base.as_deref() == Some(tip)
            {
                return Ok(true);
            }
            if upstream
                .as_ref()
                .is_some_and(|(_, upstream_tip)| contains(tip, upstream_tip))
                || merged_heads.iter().any(|merged| contains(tip, merged))
            {
                return Ok(true);
            }
            let remote_tip = match &remote_branch {
                Some(known) => known.clone(),
                None => {
                    let observed = if neutered(SafetyCheck::RemoteAsked) {
                        repo.resolve_ref(&format!("refs/remotes/origin/{}", session.branch))
                    } else {
                        let _span = crate::phase_telemetry::span("abandonment_safety_remote");
                        let destination = format!("refs/heads/{}", session.branch);
                        repo.remote_ref_oids("origin", std::slice::from_ref(&destination))
                            .map_err(|_| UnsafeReason::NotExamined)?
                            .remove(&destination)
                            .flatten()
                    };
                    remote_branch = Some(observed.clone());
                    observed
                }
            };
            Ok(remote_tip.is_some_and(|remote_tip| contains(tip, &remote_tip)))
        };
        if let Some(head) = &head
            && !neutered(SafetyCheck::HeadContained)
            && !contained(head)?
        {
            return Err(UnsafeReason::HeadNotContained);
        }
        if let Some(branch_tip) = &branch_tip
            && !neutered(SafetyCheck::BranchContained)
            && !contained(branch_tip)?
        {
            return Err(UnsafeReason::BranchNotContained);
        }

        // The working tree last: these walk it.
        if let Some(checkout) = &checkout {
            let (staged, unstaged) = checkout
                .tracked_change_kinds()
                .map_err(|_| UnsafeReason::NotExamined)?;
            if staged && !neutered(SafetyCheck::Staged) {
                return Err(UnsafeReason::Staged);
            }
            if unstaged && !neutered(SafetyCheck::Unstaged) {
                return Err(UnsafeReason::Unstaged);
            }
            if !neutered(SafetyCheck::IndexFlags)
                && checkout
                    .has_hidden_index_entries()
                    .map_err(|_| UnsafeReason::NotExamined)?
            {
                return Err(UnsafeReason::NotExamined);
            }
            if !neutered(SafetyCheck::Untracked)
                && checkout
                    .has_untracked_files()
                    .map_err(|_| UnsafeReason::NotExamined)?
            {
                return Err(UnsafeReason::Untracked);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UnsafeReason;

    /// Every reason has its own counter, named after its code.
    #[test]
    fn every_unsafe_reason_names_its_counter_after_its_code() {
        for reason in [
            UnsafeReason::InProgress,
            UnsafeReason::Stash,
            UnsafeReason::HeadNotContained,
            UnsafeReason::BranchNotContained,
            UnsafeReason::Staged,
            UnsafeReason::Unstaged,
            UnsafeReason::Untracked,
            UnsafeReason::Unreadable,
            UnsafeReason::NotExamined,
        ] {
            assert_eq!(
                reason.telemetry_name(),
                format!("session_abandonment_kept_unsafe_{}", reason.code())
            );
        }
    }
}
