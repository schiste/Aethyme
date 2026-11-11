//! One remote listing shared by every durability check of a bulk pass.
//!
//! Proving a commit durable asks the remote what a branch points at. Asked
//! once per remote-tracking ref of every retained worktree, that was a
//! network round trip per ref: measured on 2026-11-11 in a repository with
//! 448 retained worktrees and 3,582 remote-tracking refs, 46% of a cleanup
//! audit's wall time, and unbounded while refs the remote had deleted kept
//! being asked about. Listing every branch costs the same round trip as
//! asking about one.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::Broker;

/// How long a pass acts on one listing before reading the remote again. The
/// answers authorize removing a directory, so a pass that runs for minutes
/// must not act on a branch the remote dropped at its start. A plan is
/// already re-made before it is applied, so the check-to-removal gap was
/// never shorter than one pass over the eligible worktrees.
const REMOTE_HEADS_MAX_AGE: Duration = Duration::from_secs(30);
/// One listing of every branch, where a single-branch lookup allows 5 s.
const REMOTE_HEADS_LISTING_BUDGET: Duration = Duration::from_secs(10);

type RemoteHeads = Arc<BTreeMap<String, String>>;

/// What each remote listed, while a bulk pass is open.
pub(crate) struct RemoteHeadsSnapshot {
    pub(super) max_age: Duration,
    /// `None` when the remote did not answer: unknown, never absent.
    listings: BTreeMap<String, (Instant, Option<RemoteHeads>)>,
    remote_names: Option<Vec<String>>,
}

impl Default for RemoteHeadsSnapshot {
    fn default() -> Self {
        Self {
            max_age: REMOTE_HEADS_MAX_AGE,
            listings: BTreeMap::new(),
            remote_names: None,
        }
    }
}

/// Closes the pass that opened it; a nested pass leaves the outer one open.
pub(super) struct RemoteHeadsPass<'a> {
    slot: &'a std::cell::RefCell<Option<RemoteHeadsSnapshot>>,
    opened: bool,
}

impl Drop for RemoteHeadsPass<'_> {
    fn drop(&mut self) {
        if self.opened {
            self.slot.replace(None);
        }
    }
}

impl Broker {
    /// Answer remote-freshness checks from shared listings until the guard
    /// drops. Outside a pass every check asks the remote itself.
    pub(super) fn remote_heads_pass(&self) -> RemoteHeadsPass<'_> {
        let mut slot = self.remote_heads.borrow_mut();
        let opened = slot.is_none();
        if opened {
            *slot = Some(RemoteHeadsSnapshot::default());
        }
        RemoteHeadsPass {
            slot: &self.remote_heads,
            opened,
        }
    }

    /// The branches `remote` lists, when a pass is open: the outer `None` is
    /// "no pass", the inner one "the remote did not answer".
    fn remote_heads_in_pass(&self, remote: &str) -> Option<Option<RemoteHeads>> {
        let mut slot = self.remote_heads.borrow_mut();
        let snapshot = slot.as_mut()?;
        if let Some((read_at, heads)) = snapshot.listings.get(remote)
            && read_at.elapsed() < snapshot.max_age
        {
            return Some(heads.clone());
        }
        let heads = self
            .repo
            .remote_branch_heads(remote, REMOTE_HEADS_LISTING_BUDGET)
            .map(Arc::new);
        snapshot
            .listings
            .insert(remote.to_string(), (Instant::now(), heads.clone()));
        Some(heads)
    }

    /// What `remote` says `branch` points at, from the open pass. The outer
    /// `None` is "no pass": the caller asks the remote itself.
    pub(super) fn remote_branch_head_in_pass(
        &self,
        remote: &str,
        branch: &str,
    ) -> Option<Option<String>> {
        let heads = self.remote_heads_in_pass(remote)?;
        Some(heads.and_then(|heads| heads.get(branch).cloned()))
    }

    /// The remote-tracking ref names of every branch some remote lists at
    /// exactly `commit`, from the open pass; empty outside one.
    ///
    /// These are candidates only. A listing is not proof by itself: a remote
    /// whose URL is this repository lists its local branches, so the caller
    /// must still find the tracking ref a push or fetch left at that commit.
    pub(super) fn remote_refs_listed_at_in_pass(&self, commit: &str) -> Vec<String> {
        self.remote_refs_listed_at(commit).unwrap_or_default()
    }

    fn remote_refs_listed_at(&self, commit: &str) -> Option<Vec<String>> {
        let remotes = {
            let mut slot = self.remote_heads.borrow_mut();
            let snapshot = slot.as_mut()?;
            if snapshot.remote_names.is_none() {
                snapshot.remote_names = self.repo.remotes().ok();
            }
            snapshot.remote_names.clone()?
        };
        let mut refs = Vec::new();
        for remote in &remotes {
            let Some(Some(heads)) = self.remote_heads_in_pass(remote) else {
                continue;
            };
            refs.extend(
                heads
                    .iter()
                    .filter(|(_, head)| head.as_str() == commit)
                    .map(|(branch, _)| format!("refs/remotes/{remote}/{branch}")),
            );
        }
        Some(refs)
    }
}
