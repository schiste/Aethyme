//! Keep a session on the current default branch.
//!
//! Sessions deliver through their own pushed branches (#433), so the default
//! branch moves under them whenever another pull request merges. #450 stopped
//! `start` from cutting sessions from a stale integration branch, but its base
//! was still "the default branch as last fetched": in a repository nobody had
//! fetched for a day, a new worktree started a day old while reporting that it
//! started from `origin/main`.
//!
//! Two things close that:
//!
//! - `start` fetches exactly the default branch before choosing a base, so a
//!   new worktree starts from the remote's tip; if the fetch fails it starts
//!   from the cached copy and says how old that is. `start --reuse` and
//!   `--adopt` refresh it too and report how far the worktree has drifted, the
//!   same comparison `push` makes (#462).
//! - `broker sync` brings a session up to the fetched default branch when that
//!   is safe -- a clean tree and a merge Git can make without conflicts --
//!   rebasing an unpublished branch and merging into a published one, so a
//!   pull request's history is never rewritten. On a conflict it changes
//!   nothing and names the paths. Sync is local only: it moves the session
//!   branch in its worktree and never pushes; `broker push` publishes it.

use std::path::Path;

use crate::git::GitRepo;
use crate::main_drift::CatchUp;
use crate::session_push::{TrackedDefault, tracked_default};
use crate::{BrokerOpError, DefaultBranchDrift, Session, SessionStartBase};

/// How `broker sync` brought a session up to the default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStrategy {
    /// The branch was not published: its commits were replayed onto the
    /// default branch.
    Rebase,
    /// The branch is published: the default branch was merged into it, so
    /// commits a pull request already shows keep their identity.
    Merge,
    /// Nothing was changed: already current, or merging would conflict.
    None,
}

/// Why a sync ended where it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncOutcome {
    /// The branch already contained the default branch.
    AlreadyCurrent,
    /// The branch was rebased or merged and now contains it.
    Synced,
    /// Merging would conflict; the worktree was left untouched.
    Conflict,
}

/// The result of `broker sync`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSyncReport {
    pub session_id: i64,
    pub branch: String,
    pub outcome: SyncOutcome,
    pub strategy: SyncStrategy,
    /// The session head before the sync.
    pub before: String,
    /// The session head after it (equal to `before` unless synced).
    pub after: String,
    /// The default branch compared against, e.g. `origin/main`, and its tip.
    pub default_ref: String,
    pub default_commit: String,
    /// Default-branch commits the branch was missing before the sync.
    pub behind_before: u64,
    /// Session commits the default branch does not contain.
    pub ahead: u64,
    /// Up to five conflicting paths, when the merge would conflict.
    pub conflicts: Vec<String>,
    /// Whether the default branch was refreshed from its remote first.
    pub fetched: bool,
    /// Why it was not, when `fetched` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch_note: Option<String>,
    /// What to run by hand, when the sync stopped at a conflict.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub manual_commands: Vec<String>,
    /// The command that publishes a synced branch. A sync only moves the
    /// local session branch; it never pushes, so after `synced` the remote
    /// still holds the old head (or no branch at all) until this runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

fn sync_refused(reason: impl Into<String>) -> BrokerOpError {
    BrokerOpError::SessionSyncRefused {
        reason: reason.into(),
    }
}

/// How long `start` waits for its default-branch refresh. Starting a session
/// must cost seconds, not a network timeout; the cached copy is always a safe
/// fallback, and the report says how old it is.
const START_REFRESH_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// What `start` learned refreshing the default branch before choosing a base.
pub(crate) struct StartRefresh {
    fetched: Option<bool>,
    error: Option<String>,
    cached_ref_age_seconds: Option<u64>,
}

impl StartRefresh {
    /// Copy the refresh outcome onto the chosen base.
    pub(crate) fn record(self, base: &mut SessionStartBase) {
        base.fetched = self.fetched;
        base.fetch_error = self.error;
        base.cached_ref_age_seconds = self.cached_ref_age_seconds;
    }
}

impl crate::Broker {
    /// Fetch exactly the default branch before `start` chooses a base, so a
    /// new worktree starts from the remote's tip rather than the last fetch.
    ///
    /// This runs before the session exists, so it cannot go through the
    /// coordinated operation lane: every coordinated operation is recorded
    /// against a session row, and a session's provenance anchor
    /// (`adopted_head`) is immutable once registered, so the base cannot be
    /// moved after the fact. It is a bounded read of one remote-tracking ref,
    /// which Git's own ref locking keeps safe against a concurrent fetch.
    /// `push`, `sync` and `start --reuse/--adopt` have a session and use the
    /// coordinated lane. A failure is never fatal: the base comes from the
    /// cached copy and the report says how old it is.
    pub(crate) fn refresh_default_branch_before_start(&self) -> StartRefresh {
        let repo = self.repo_handle();
        let Some(default) = tracked_default(repo) else {
            return StartRefresh {
                fetched: None,
                error: None,
                cached_ref_age_seconds: None,
            };
        };
        match repo.fetch_branch_into_tracking_ref(
            &default.remote,
            &default.branch,
            START_REFRESH_BUDGET,
        ) {
            Ok(()) => StartRefresh {
                fetched: Some(true),
                error: None,
                cached_ref_age_seconds: None,
            },
            Err(error) => StartRefresh {
                fetched: Some(false),
                error: Some(format!(
                    "fetching {} failed ({})",
                    default.tracking_ref,
                    error.to_string().lines().next().unwrap_or("unknown error")
                )),
                cached_ref_age_seconds: repo.ref_age_seconds(&default.tracking_ref),
            },
        }
    }

    /// Refresh the default branch and compare a reused or adopted session's
    /// head with it, exactly as `push` does.
    pub(crate) fn reused_session_drift(
        &mut self,
        session: &Session,
    ) -> (Option<DefaultBranchDrift>, Option<String>) {
        let main_root = self.main_root().to_path_buf();
        let Some(default) = tracked_default(self.repo_handle()) else {
            return (None, None);
        };
        let Some(head) = GitRepo::discover(Path::new(&session.worktree_path))
            .ok()
            .and_then(|worktree| worktree.head_commit().ok())
        else {
            return (
                None,
                Some("the session worktree has no readable HEAD".into()),
            );
        };
        self.fetch_and_measure_default_branch(
            session.id,
            &session.branch,
            &default,
            None,
            &head,
            &main_root,
        )
    }

    /// Bring a live session up to the freshly fetched default branch when it
    /// is safe; see the module documentation.
    pub fn sync_session(&mut self, session_id: i64) -> Result<SessionSyncReport, BrokerOpError> {
        let session = self.store().session(session_id)?;
        if session.status.is_closed() {
            return Err(BrokerOpError::ClosedSessionOperation {
                session_id,
                repository_root: self.main_root().display().to_string(),
            });
        }
        let path = Path::new(&session.worktree_path);
        let worktree = GitRepo::discover(path).map_err(|error| {
            sync_refused(format!(
                "session {session_id}'s worktree {} is not a Git checkout ({error})",
                path.display()
            ))
        })?;
        if let Some(operation) = crate::git::worktree_operation_in_progress(path) {
            return Err(sync_refused(format!(
                "a {operation} is already in progress in {}; finish or abort it first",
                path.display()
            )));
        }
        let uncommitted = worktree.uncommitted_summary()?;
        if !uncommitted.modified.is_empty() || !uncommitted.untracked.is_empty() {
            let sample = uncommitted
                .modified
                .iter()
                .chain(&uncommitted.untracked)
                .take(5)
                .cloned()
                .collect::<Vec<_>>();
            return Err(sync_refused(format!(
                "the worktree has {} modified and {} untracked entr(ies) ({}); commit or \
                 set them aside, then sync",
                uncommitted.modified.len(),
                uncommitted.untracked.len(),
                sample.join(", ")
            )));
        }
        let default: TrackedDefault = tracked_default(self.repo_handle()).ok_or_else(|| {
            sync_refused(
                "the main checkout tracks no fetched default branch; set origin/HEAD with \
                 `git remote set-head origin --auto`",
            )
        })?;
        let main_root = self.main_root().to_path_buf();
        let fetch_note = self
            .fetch_default_branch(session_id, &default, None, &main_root)
            .err();
        let default_commit = self
            .repo_handle()
            .resolve_ref(&default.tracking_ref)
            .ok_or_else(|| sync_refused(format!("{} is not fetched", default.tracking_ref)))?;
        let default_ref = format!("{}/{}", default.remote, default.branch);
        let before = worktree.head_commit()?;
        let catch_up = CatchUp::for_branch(self.repo_handle(), &default.remote, &session.branch);
        let drift =
            crate::main_drift::measure(&worktree, &default_ref, &default_commit, &before, catch_up)
                .ok_or_else(|| {
                    sync_refused(format!("could not compare the session with {default_ref}"))
                })?;
        let mut report = SessionSyncReport {
            session_id,
            branch: session.branch.clone(),
            outcome: SyncOutcome::AlreadyCurrent,
            strategy: SyncStrategy::None,
            before: before.clone(),
            after: before.clone(),
            default_ref: default_ref.clone(),
            default_commit: default_commit.clone(),
            behind_before: drift.behind,
            ahead: drift.ahead,
            conflicts: drift.conflicting_paths.clone(),
            fetched: fetch_note.is_none(),
            fetch_note,
            manual_commands: Vec::new(),
            next_action: None,
        };
        if drift.behind == 0 {
            return Ok(report);
        }
        if drift.would_conflict {
            report.outcome = SyncOutcome::Conflict;
            report.manual_commands = vec![
                format!("cd {}", shell_quote(&session.worktree_path)),
                match catch_up {
                    CatchUp::Merge => format!("git merge {default_ref}"),
                    CatchUp::Rebase => format!("git rebase {default_ref}"),
                },
                "# resolve the conflicts, commit, then: aethyme broker push --session \
                 <id>"
                    .replace("<id>", &session_id.to_string()),
            ];
            return Ok(report);
        }
        let applied = match catch_up {
            CatchUp::Rebase => worktree.rebase_onto(&default_commit).inspect_err(|_| {
                crate::warn_unrecorded("abort a failed sync rebase", worktree.abort_rebase());
            }),
            CatchUp::Merge => worktree
                .merge_commit_no_edit(
                    &default_commit,
                    &format!("Merge {default_ref} into {}", session.branch),
                )
                .inspect_err(|_| {
                    crate::warn_unrecorded("abort a failed sync merge", worktree.abort_merge());
                }),
        };
        applied?;
        let after = worktree.head_commit()?;
        self.store()
            .set_session_diff_base(session_id, &default_commit)?;
        report.outcome = SyncOutcome::Synced;
        report.strategy = match catch_up {
            CatchUp::Merge => SyncStrategy::Merge,
            CatchUp::Rebase => SyncStrategy::Rebase,
        };
        report.after = after.clone();
        report.next_action = Some(format!("aethyme broker push --session {session_id}"));
        let payload = serde_json::json!({
            "session_id": session_id,
            "branch": session.branch,
            "strategy": report.strategy,
            "from": before,
            "to": after,
            "default_ref": default_ref,
            "default_commit": default_commit,
            "behind_before": report.behind_before,
        })
        .to_string();
        self.store().append_event(
            crate::events::BROKER_SESSION_SYNCED,
            Some(session_id),
            Some(&payload),
        )?;
        Ok(report)
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}
