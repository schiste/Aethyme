//! One repository's retained worktrees, judged by content against a named
//! snapshot of the authoritative target (#354).
//!
//! The cleanup plan answers "what may be removed" with one label per session,
//! and anything it cannot prove by ancestry or a recorded representation
//! collapses into `unproven_provenance`. That label covers work that landed
//! through a squash merge, work that only reached the integration branch, and
//! work that exists nowhere else -- three situations with three different next
//! actions. An operator separating them had to combine the plan, integration
//! reconciliation, `git worktree list` and the default branch's history by
//! hand, and a stale local `main` quietly made every answer wrong.
//!
//! The audit separates them. It names the exact commits it judged against,
//! prefers the fetched upstream over a local branch that trails it, and
//! recognises delivery by ancestry, by net content (squash), and by patch
//! identity (rebase or cherry-pick).
//!
//! Strictly read-only. It removes nothing and grants nothing: the only way to
//! act on it is the existing cleanup apply, whose digest the audit reports and
//! which binds the same target snapshot, so a target that moves after review
//! invalidates the confirmation instead of widening what it removes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::git::{GitRepo, GitWorktreeInfo};
use crate::representation::{self, LandingOutcome};
use crate::{Broker, BrokerOpError, CleanupPlan, CleanupWorktreePlan, Session, SessionOrigin};

pub const CLEANUP_AUDIT_SCHEMA_VERSION: u32 = 1;

/// The refs the audit judged against, resolved once so every item is compared
/// to the same commits.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AuditTarget {
    /// The authoritative branch name, e.g. `main`.
    pub branch: String,
    /// How the branch was chosen: `origin_head` when `refs/remotes/origin/HEAD`
    /// names it, `primary_checkout` when the primary checkout's branch had to
    /// stand in.
    pub branch_source: &'static str,
    pub local_ref: String,
    pub local_commit: Option<String>,
    pub upstream_ref: Option<String>,
    pub upstream_commit: Option<String>,
    pub integration_ref: String,
    pub integration_commit: Option<String>,
    /// The ref every "in target" verdict was proven against.
    pub proof_ref: String,
    pub proof_commit: String,
    /// Commits the upstream holds that the local branch does not.
    pub local_behind_upstream: u64,
    /// Commits the local branch holds that the upstream does not.
    pub local_ahead_upstream: u64,
    pub warnings: Vec<String>,
}

/// Who, if anyone, accounts for an item.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditOwner {
    Session {
        session_id: i64,
        status: String,
        origin: SessionOrigin,
    },
    /// No broker session row names this path.
    None,
}

/// What exists on disk and in Git's worktree registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutState {
    /// A directory Git lists as a worktree.
    Present,
    /// A session's directory is gone. The branch may still carry its work.
    MissingDirectory,
    /// A directory on disk that Git no longer lists: its administrative
    /// metadata is gone, typically after an interrupted removal.
    MissingGitMetadata,
    /// Git still lists a worktree whose directory is gone and no session owns
    /// it. Only administrative metadata remains.
    PrunableRegistration,
    /// A directory under a broker worktree root that no session row claims.
    UnownedRoot,
}

impl CheckoutState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::MissingDirectory => "missing_directory",
            Self::MissingGitMetadata => "missing_git_metadata",
            Self::PrunableRegistration => "prunable_registration",
            Self::UnownedRoot => "unowned_root",
        }
    }
}

/// How a commit reached a ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingEvidence {
    /// The commit is an ancestor (fast-forward or merge delivery).
    Ancestry,
    /// A commit on the ref carries the net content (squash delivery).
    Content,
    /// Every commit's patch is on the ref under another SHA (rebase delivery).
    PatchEquivalent,
    /// The commit changes nothing the ref does not already hold.
    NoNetChange,
}

impl LandingEvidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ancestry => "ancestry",
            Self::Content => "content",
            Self::PatchEquivalent => "patch equivalence",
            Self::NoNetChange => "no net change",
        }
    }
}

/// Where a checkout's committed work is represented.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CommittedWork {
    InTarget {
        evidence: LandingEvidence,
        #[serde(skip_serializing_if = "Option::is_none")]
        landed_by: Option<String>,
    },
    /// Not on the proof ref, but on the integration branch or the local
    /// target branch.
    IntegrationOnly {
        refs: Vec<String>,
    },
    /// Not in the target or integration, but reachable from a remote branch
    /// (typically an open pull request's head), so not lost with the checkout.
    RemoteBranchOnly {
        refs: Vec<String>,
    },
    /// Commits whose content exists only on this checkout's head or branch.
    WorktreeOnly {
        unique_commits: u64,
    },
    Unknown {
        reason: String,
    },
}

impl CommittedWork {
    /// Higher is more to lose; the worst of a head and a diverging branch
    /// speaks for the item.
    fn severity(&self) -> u8 {
        match self {
            Self::InTarget { .. } => 0,
            Self::IntegrationOnly { .. } => 1,
            Self::RemoteBranchOnly { .. } => 2,
            Self::Unknown { .. } => 3,
            Self::WorktreeOnly { .. } => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditDisposition {
    /// A session is using the checkout.
    Live,
    /// Uncommitted tracked or untracked changes.
    Dirty,
    /// Commits exist only here.
    WorktreeOnly,
    /// Nothing records what the item held, or it could not be inspected.
    UnknownProvenance,
    /// Durable on a remote branch, not in the target or integration.
    RemoteBranchOnly,
    /// Represented on integration or the local branch, not the proof ref.
    IntegrationOnly,
    /// Only Git administrative metadata or an empty session record remains.
    MissingCheckoutMetadata,
    /// Represented on the proof ref.
    InTarget,
}

impl AuditDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Dirty => "dirty",
            Self::WorktreeOnly => "worktree_only",
            Self::UnknownProvenance => "unknown_provenance",
            Self::RemoteBranchOnly => "remote_branch_only",
            Self::IntegrationOnly => "integration_only",
            Self::MissingCheckoutMetadata => "missing_checkout_metadata",
            Self::InTarget => "in_target",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AuditItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub owner: AuditOwner,
    pub checkout: CheckoutState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_tip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// Tracked paths with staged or unstaged changes.
    pub tracked_changes: usize,
    /// Untracked, unignored paths.
    pub untracked_files: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed: Option<CommittedWork>,
    /// Measured bytes on disk; absent when not measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    pub disposition: AuditDisposition,
    /// Whether the reviewed cleanup apply would remove this item.
    pub removable: bool,
    pub evidence: Vec<String>,
    /// Why the item is retained. Absent only when `removable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    pub next_action: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct AuditSummary {
    pub item_count: usize,
    /// Count per disposition, keyed by its snake_case name. Every disposition
    /// is present, so an absent zero is never mistaken for an unchecked one.
    pub by_disposition: BTreeMap<String, usize>,
    pub removable_count: usize,
    pub measured_bytes: u64,
    pub removable_bytes: u64,
    pub unmeasured_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupAudit {
    pub schema_version: u32,
    pub repository: String,
    pub target: AuditTarget,
    pub summary: AuditSummary,
    /// The cleanup plan digest this audit was assembled with. It covers the
    /// target snapshot, so it stops matching once any target ref moves.
    pub cleanup_plan_digest: String,
    /// The one command that removes the removable items, after revalidating
    /// the reviewed snapshot. Absent when nothing is removable, or when the
    /// plan would remove something this audit retains.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_command: Option<String>,
    pub warnings: Vec<String>,
    pub items: Vec<AuditItem>,
}

fn short(sha: &str) -> &str {
    &sha[..12.min(sha.len())]
}

fn path_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

impl Broker {
    /// `ref=commit` for every ref cleanup provenance is judged against, sorted.
    /// The cleanup plan hashes this, so its digest pins the targets.
    pub(crate) fn cleanup_target_snapshot(&self) -> Vec<String> {
        let repo = self.repo_handle();
        let mut refs = BTreeSet::new();
        if let Ok(head) = repo.head_commit() {
            refs.insert(format!("HEAD={head}"));
        }
        let integration = crate::merge::PromoteConfig::load(&self.main_root_path()).branch;
        if let Some(commit) = self.integration_tip() {
            refs.insert(format!("refs/heads/{integration}={commit}"));
        }
        if let Some((upstream, commit)) = repo.tracking_upstream() {
            refs.insert(format!("{upstream}={commit}"));
        }
        if let Some(branch) = self.origin_default_branch() {
            for name in [
                format!("refs/heads/{branch}"),
                format!("refs/remotes/origin/{branch}"),
            ] {
                if let Some(commit) = repo.resolve_ref(&name) {
                    refs.insert(format!("{name}={commit}"));
                }
            }
        }
        refs.into_iter().collect()
    }

    fn origin_default_branch(&self) -> Option<String> {
        self.repo_handle()
            .symbolic_ref("refs/remotes/origin/HEAD")
            .and_then(|head| {
                head.strip_prefix("refs/remotes/origin/")
                    .map(str::to_string)
            })
    }

    /// Resolve the authoritative target and choose the proof ref.
    ///
    /// The fetched upstream is preferred whenever it exists: a local branch
    /// that trails it would call landed work unique, and one that leads it
    /// holds commits nobody else has. Either way the gap is named rather than
    /// silently adopted as the baseline. Nothing is fetched.
    pub fn cleanup_audit_target(&self) -> Result<AuditTarget, BrokerOpError> {
        let repo = self.repo_handle();
        let (branch, branch_source) = match self.origin_default_branch() {
            Some(branch) => (branch, "origin_head"),
            None => (repo.current_branch()?, "primary_checkout"),
        };
        let local_ref = format!("refs/heads/{branch}");
        let local_commit = repo.resolve_ref(&local_ref);
        let upstream_ref = format!("refs/remotes/origin/{branch}");
        let upstream_commit = repo.resolve_ref(&upstream_ref);
        let integration_branch = crate::merge::PromoteConfig::load(&self.main_root_path()).branch;
        let integration_ref = format!("refs/heads/{integration_branch}");
        let integration_commit = self.integration_tip();

        let mut warnings = Vec::new();
        let (mut behind, mut ahead) = (0, 0);
        if let (Some(local), Some(upstream)) = (&local_commit, &upstream_commit) {
            behind = repo.commit_count_between(local, upstream).unwrap_or(0);
            ahead = repo.commit_count_between(upstream, local).unwrap_or(0);
        }
        let (proof_ref, proof_commit) = match (&upstream_commit, &local_commit) {
            (Some(upstream), _) => (upstream_ref.clone(), upstream.clone()),
            (None, Some(local)) => {
                warnings.push(format!(
                    "no fetched {upstream_ref}; proving against local {local_ref}, which may be stale -- `git fetch origin` and re-run to prove against the remote"
                ));
                (local_ref.clone(), local.clone())
            }
            (None, None) => {
                return Err(BrokerOpError::RepresentationUnavailable {
                    reason: format!("neither {local_ref} nor {upstream_ref} resolves"),
                });
            }
        };
        if behind > 0 {
            warnings.push(format!(
                "local {branch} is {behind} commit(s) behind {upstream_ref}; the audit proves against {upstream_ref} at {}, not the stale local branch",
                short(&proof_commit)
            ));
        }
        if ahead > 0 {
            warnings.push(format!(
                "local {branch} holds {ahead} commit(s) {upstream_ref} does not; work only there is reported integration_only, not in_target"
            ));
        }
        Ok(AuditTarget {
            branch,
            branch_source,
            local_ref,
            local_commit,
            upstream_ref: upstream_commit.as_ref().map(|_| upstream_ref.clone()),
            upstream_commit,
            integration_ref,
            integration_commit,
            proof_ref,
            proof_commit,
            local_behind_upstream: behind,
            local_ahead_upstream: ahead,
            warnings,
        })
    }

    /// How `head` reached `tip`, if it did.
    fn landing_evidence(
        &self,
        head: &str,
        tip: &str,
    ) -> Result<Option<(LandingEvidence, Option<String>)>, BrokerOpError> {
        let repo = self.repo_handle();
        if repo.is_ancestor(head, tip) {
            return Ok(Some((LandingEvidence::Ancestry, None)));
        }
        let base = repo.merge_base(head, tip)?;
        let content = representation::session_content(repo, &base, head)?;
        let search =
            representation::find_landing(repo, &content, tip, representation::DEFAULT_SEARCH_CAP)?;
        match search.outcome {
            LandingOutcome::NothingToRepresent => {
                return Ok(Some((LandingEvidence::NoNetChange, None)));
            }
            LandingOutcome::Landed(landing) => {
                return Ok(Some((LandingEvidence::Content, Some(landing.commit))));
            }
            LandingOutcome::NotFound { .. } => {}
        }
        if repo.patch_unique_commit_count(tip, head)? == 0 {
            return Ok(Some((LandingEvidence::PatchEquivalent, None)));
        }
        Ok(None)
    }

    fn classify_committed(&self, head: &str, target: &AuditTarget) -> CommittedWork {
        let result = (|| -> Result<CommittedWork, BrokerOpError> {
            if let Some((evidence, landed_by)) =
                self.landing_evidence(head, &target.proof_commit)?
            {
                return Ok(CommittedWork::InTarget {
                    evidence,
                    landed_by,
                });
            }
            let mut refs = Vec::new();
            let others = [
                (&target.local_ref, &target.local_commit),
                (&target.integration_ref, &target.integration_commit),
            ];
            for (name, commit) in others {
                if let Some(commit) = commit
                    && commit != &target.proof_commit
                    && self.landing_evidence(head, commit)?.is_some()
                {
                    refs.push(name.clone());
                }
            }
            if !refs.is_empty() {
                return Ok(CommittedWork::IntegrationOnly { refs });
            }
            // Local remote-tracking refs only; nothing is fetched. The cleanup
            // plan separately verifies such a ref is current before relying on
            // it, and removal still requires the plan to agree.
            let remote_refs = self.repo_handle().remote_refs_containing(head);
            if !remote_refs.is_empty() {
                return Ok(CommittedWork::RemoteBranchOnly { refs: remote_refs });
            }
            let unique = self
                .repo_handle()
                .patch_unique_commit_count(&target.proof_commit, head)?;
            Ok(CommittedWork::WorktreeOnly {
                unique_commits: unique,
            })
        })();
        result.unwrap_or_else(|error| CommittedWork::Unknown {
            reason: error.to_string(),
        })
    }

    /// The worst verdict over a checkout's head and its branch tip, which can
    /// diverge when the checkout is detached.
    fn classify_heads(&self, heads: &[&str], target: &AuditTarget) -> Option<CommittedWork> {
        let mut seen = BTreeSet::new();
        heads
            .iter()
            .filter(|head| seen.insert(**head))
            .map(|head| self.classify_committed(head, target))
            .max_by_key(CommittedWork::severity)
    }

    /// Read-only, repository-scoped cleanup audit (#354).
    ///
    /// Covers every session row whose checkout or branch still exists, every
    /// Git worktree registration of this repository, and every directory
    /// under this repository's broker worktree roots. Nothing outside those
    /// is walked, so unrelated repositories on the host are never scanned.
    pub fn cleanup_audit(&self) -> Result<CleanupAudit, BrokerOpError> {
        let target = self.cleanup_audit_target()?;
        let plan = self.cleanup_plan()?;
        let repo = self.repo_handle();
        let main_root = path_key(&self.main_root_path());
        let inventory = repo.worktree_inventory().map_err(BrokerOpError::from);
        let mut warnings = target.warnings.clone();
        let registered: Vec<GitWorktreeInfo> = match &inventory {
            Ok(entries) => entries.clone(),
            Err(error) => {
                warnings.push(format!(
                    "git worktree inventory unreadable ({error}); registration state is unknown"
                ));
                Vec::new()
            }
        };
        let registration = |path: &Path| {
            let key = path_key(path);
            registered.iter().find(|entry| path_key(&entry.path) == key)
        };

        let plan_items: BTreeMap<i64, &CleanupWorktreePlan> = plan
            .worktrees
            .iter()
            .map(|item| (item.session_id, item))
            .collect();
        let mut claimed = BTreeSet::new();
        let mut items = Vec::new();

        let store = self.store_ref();
        let sessions = store
            .live_sessions()?
            .into_iter()
            .chain(store.cleaned_sessions()?);
        for session in sessions {
            let path = PathBuf::from(&session.worktree_path);
            if path_key(&path) == main_root {
                continue;
            }
            claimed.insert(path_key(&path));
            if let Some(item) = self.audit_session(
                &session,
                &path,
                registration(&path),
                inventory.is_ok(),
                plan_items.get(&session.id).copied(),
                &target,
            ) {
                items.push(item);
            }
        }

        for entry in &registered {
            let key = path_key(&entry.path);
            if key == main_root || entry.bare || claimed.contains(&key) {
                continue;
            }
            claimed.insert(key);
            items.push(self.audit_unowned_registration(entry, &target));
        }

        let reconciliation = self.reconcile_worktree_directories(true)?;
        for directory in &reconciliation.unclaimed {
            let path = PathBuf::from(&directory.path);
            if claimed.contains(&path_key(&path)) {
                continue;
            }
            items.push(AuditItem {
                path: Some(directory.path.clone()),
                owner: AuditOwner::None,
                checkout: CheckoutState::UnownedRoot,
                branch: None,
                branch_tip: None,
                head: None,
                tracked_changes: 0,
                untracked_files: 0,
                committed: None,
                bytes: directory.estimated_bytes,
                disposition: AuditDisposition::UnknownProvenance,
                removable: false,
                evidence: vec![format!(
                    "{} under a broker worktree root; no session row and no Git registration names it",
                    directory.kind
                )],
                blocker: Some(
                    "no broker session or Git registration records what this directory held (#257)"
                        .into(),
                ),
                next_action: format!(
                    "inspect `{}` by hand; the broker will not remove a directory it has no record of",
                    directory.path
                ),
            });
        }

        items.sort_by(|a, b| {
            a.disposition
                .cmp(&b.disposition)
                .then_with(|| b.bytes.cmp(&a.bytes))
                .then_with(|| a.path.cmp(&b.path))
        });

        // The plan's apply removes everything it calls eligible. If the audit
        // retains any of those, printing the apply would authorize removing
        // what this report says to keep.
        let disagreements: Vec<i64> = items
            .iter()
            .filter_map(|item| match item.owner {
                AuditOwner::Session { session_id, .. }
                    if !item.removable
                        && plan_items
                            .get(&session_id)
                            .is_some_and(|plan| plan.eligible()) =>
                {
                    Some(session_id)
                }
                _ => None,
            })
            .collect();
        let summary = summarise(&items);
        let apply_command = if !disagreements.is_empty() {
            warnings.push(format!(
                "the cleanup plan calls session(s) {} eligible but this audit retains them against {}; no apply command is offered until they agree",
                disagreements
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                target.proof_ref
            ));
            None
        } else if summary.removable_count > 0 {
            Some(apply_command(&plan))
        } else {
            None
        };

        Ok(CleanupAudit {
            schema_version: CLEANUP_AUDIT_SCHEMA_VERSION,
            repository: self.main_root_path().to_string_lossy().into_owned(),
            target,
            summary,
            cleanup_plan_digest: plan.digest.clone(),
            apply_command,
            warnings,
            items,
        })
    }

    fn audit_session(
        &self,
        session: &Session,
        path: &Path,
        registration: Option<&GitWorktreeInfo>,
        inventory_known: bool,
        plan_item: Option<&CleanupWorktreePlan>,
        target: &AuditTarget,
    ) -> Option<AuditItem> {
        let repo = self.repo_handle();
        let present = path.exists();
        let branch_tip = repo.resolve_ref(&format!("refs/heads/{}", session.branch));
        let live = !session.status.is_closed();
        if !present && branch_tip.is_none() && !live && registration.is_none() {
            // Nothing on disk, no branch, no registration: the row is history.
            return None;
        }
        let checkout = match (present, registration.is_some() || !inventory_known) {
            (true, true) => CheckoutState::Present,
            (true, false) => CheckoutState::MissingGitMetadata,
            (false, _) => CheckoutState::MissingDirectory,
        };
        let mut evidence = Vec::new();
        let mut head = None;
        let (mut tracked_changes, mut untracked_files) = (0, 0);
        let mut inspection_error = None;
        if checkout == CheckoutState::Present {
            match GitRepo::discover(path).and_then(|checkout| {
                Ok((
                    checkout.head_commit()?,
                    checkout.tracked_dirty_paths()?.len(),
                    checkout.untracked_paths()?.len(),
                ))
            }) {
                Ok((commit, tracked, untracked)) => {
                    head = Some(commit);
                    tracked_changes = tracked;
                    untracked_files = untracked;
                }
                Err(error) => inspection_error = Some(error.to_string()),
            }
        }
        if checkout == CheckoutState::MissingDirectory && registration.is_some() {
            evidence.push("Git still registers the missing directory (prunable)".into());
        }
        if checkout == CheckoutState::MissingGitMetadata {
            // The cleanup plan judges this case from the branch alone (#165):
            // an interrupted removal already deleted part of the tree, so the
            // directory describes nothing. Say so rather than report zero
            // changes as if they had been counted.
            evidence.push(
                "Git no longer registers this directory, so its uncommitted state cannot be read; only the branch is judged".into(),
            );
        }
        let heads: Vec<&str> = head
            .iter()
            .chain(branch_tip.iter())
            .map(String::as_str)
            .collect();
        let committed = self.classify_heads(&heads, target);
        if let Some(committed) = &committed {
            evidence.push(describe_committed(committed, target));
        }
        let plan_eligible = plan_item.is_some_and(CleanupWorktreePlan::eligible);
        if let Some(plan_item) = plan_item {
            evidence.push(format!(
                "cleanup plan: {} -- {}",
                plan_item.disposition.as_str(),
                plan_item.reason
            ));
        }

        let disposition = if live {
            AuditDisposition::Live
        } else if inspection_error.is_some() {
            AuditDisposition::UnknownProvenance
        } else if tracked_changes > 0 || untracked_files > 0 {
            AuditDisposition::Dirty
        } else {
            match &committed {
                Some(CommittedWork::InTarget { .. }) => AuditDisposition::InTarget,
                Some(CommittedWork::IntegrationOnly { .. }) => AuditDisposition::IntegrationOnly,
                Some(CommittedWork::RemoteBranchOnly { .. }) => AuditDisposition::RemoteBranchOnly,
                Some(CommittedWork::WorktreeOnly { .. }) => AuditDisposition::WorktreeOnly,
                Some(CommittedWork::Unknown { .. }) => AuditDisposition::UnknownProvenance,
                None if checkout == CheckoutState::MissingDirectory => {
                    AuditDisposition::MissingCheckoutMetadata
                }
                None => AuditDisposition::UnknownProvenance,
            }
        };
        let removable = plan_eligible
            && matches!(
                disposition,
                AuditDisposition::InTarget
                    | AuditDisposition::IntegrationOnly
                    | AuditDisposition::RemoteBranchOnly
            );
        let id = session.id;
        let (blocker, next_action) = if removable {
            (
                None,
                "remove with the reviewed cleanup apply (see apply_command)".to_string(),
            )
        } else {
            session_blocker(
                session,
                path,
                disposition,
                checkout,
                inspection_error.as_deref(),
                plan_item,
                target,
                (tracked_changes, untracked_files),
                id,
            )
        };
        Some(AuditItem {
            path: Some(session.worktree_path.clone()),
            owner: AuditOwner::Session {
                session_id: id,
                status: session.status.as_str().to_string(),
                origin: session.origin,
            },
            checkout,
            branch: Some(session.branch.clone()),
            branch_tip,
            head,
            tracked_changes,
            untracked_files,
            committed,
            bytes: plan_item.and_then(|item| item.estimated_bytes),
            disposition,
            removable,
            evidence,
            blocker,
            next_action,
        })
    }

    fn audit_unowned_registration(
        &self,
        entry: &GitWorktreeInfo,
        target: &AuditTarget,
    ) -> AuditItem {
        let path = entry.path.to_string_lossy().into_owned();
        let branch = entry.branch.as_deref().map(|branch| {
            branch
                .strip_prefix("refs/heads/")
                .unwrap_or(branch)
                .to_string()
        });
        if entry.prunable || !entry.path.exists() {
            return AuditItem {
                path: Some(path),
                owner: AuditOwner::None,
                checkout: CheckoutState::PrunableRegistration,
                branch,
                branch_tip: None,
                head: entry.head.clone(),
                tracked_changes: 0,
                untracked_files: 0,
                committed: None,
                bytes: Some(0),
                disposition: AuditDisposition::MissingCheckoutMetadata,
                removable: false,
                evidence: vec![format!(
                    "Git registers this worktree but its directory is gone{}",
                    entry
                        .prunable_reason
                        .as_deref()
                        .map(|reason| format!(" ({reason})"))
                        .unwrap_or_default()
                )],
                blocker: Some(
                    "administrative metadata only; pruning it is separate from removing a checkout or branch"
                        .into(),
                ),
                next_action: "git worktree prune --dry-run --verbose, then git worktree prune once the listed entries are the expected ones".into(),
            };
        }
        let checkout = GitRepo::discover(&entry.path).and_then(|checkout| {
            Ok((
                checkout.head_commit()?,
                checkout.tracked_dirty_paths()?.len(),
                checkout.untracked_paths()?.len(),
            ))
        });
        let (head, tracked_changes, untracked_files, committed, disposition) = match checkout {
            Ok((head, tracked, untracked)) => {
                let committed = self.classify_committed(&head, target);
                let disposition = if tracked + untracked > 0 {
                    AuditDisposition::Dirty
                } else {
                    match committed {
                        CommittedWork::InTarget { .. } => AuditDisposition::InTarget,
                        CommittedWork::IntegrationOnly { .. } => AuditDisposition::IntegrationOnly,
                        CommittedWork::RemoteBranchOnly { .. } => {
                            AuditDisposition::RemoteBranchOnly
                        }
                        CommittedWork::WorktreeOnly { .. } => AuditDisposition::WorktreeOnly,
                        CommittedWork::Unknown { .. } => AuditDisposition::UnknownProvenance,
                    }
                };
                (Some(head), tracked, untracked, Some(committed), disposition)
            }
            Err(_) => (None, 0, 0, None, AuditDisposition::UnknownProvenance),
        };
        let mut evidence =
            vec!["Git registers this worktree; no broker session row names it".into()];
        if let Some(committed) = &committed {
            evidence.push(describe_committed(committed, target));
        }
        AuditItem {
            path: Some(path.clone()),
            owner: AuditOwner::None,
            checkout: CheckoutState::Present,
            branch,
            branch_tip: None,
            head,
            tracked_changes,
            untracked_files,
            committed,
            bytes: crate::broker::directory_size_without_following_links(&entry.path).ok(),
            disposition,
            removable: false,
            evidence,
            blocker: Some(
                "not owned by a broker session, so broker cleanup never removes it (#257)".into(),
            ),
            next_action: format!(
                "decide by hand: `git -C '{path}' status --short` and `git -C '{path}' log --oneline {}..HEAD`; adopt it with `aethyme broker start --adopt` to bring it under broker cleanup",
                short(&target.proof_commit)
            ),
        }
    }
}

fn describe_committed(committed: &CommittedWork, target: &AuditTarget) -> String {
    match committed {
        CommittedWork::InTarget {
            evidence,
            landed_by,
        } => match landed_by {
            Some(commit) => format!(
                "committed work is in {} by {} (landed by {})",
                target.proof_ref,
                evidence.as_str(),
                short(commit)
            ),
            None => format!(
                "committed work is in {} by {}",
                target.proof_ref,
                evidence.as_str()
            ),
        },
        CommittedWork::IntegrationOnly { refs } => format!(
            "committed work is on {} but not {}",
            refs.join(", "),
            target.proof_ref
        ),
        CommittedWork::RemoteBranchOnly { refs } => format!(
            "committed work is on remote branch {} but not {} or integration",
            refs.join(", "),
            target.proof_ref
        ),
        CommittedWork::WorktreeOnly { unique_commits } => format!(
            "{unique_commits} commit(s) exist only here, not in {} by ancestry, content or patch",
            target.proof_ref
        ),
        CommittedWork::Unknown { reason } => {
            format!("committed work could not be classified: {reason}")
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn session_blocker(
    session: &Session,
    path: &Path,
    disposition: AuditDisposition,
    checkout: CheckoutState,
    inspection_error: Option<&str>,
    plan_item: Option<&CleanupWorktreePlan>,
    target: &AuditTarget,
    (tracked, untracked): (usize, usize),
    id: i64,
) -> (Option<String>, String) {
    let quoted = format!("'{}'", path.display());
    let (blocker, next) = match disposition {
        AuditDisposition::Live => (
            format!("session {id} is {}", session.status.as_str()),
            format!("aethyme broker finish --session {id}"),
        ),
        AuditDisposition::Dirty => (
            format!("{tracked} tracked change(s) and {untracked} untracked file(s) are not in any commit"),
            format!("git -C {quoted} status --short; commit what matters, then re-run the audit"),
        ),
        AuditDisposition::WorktreeOnly => {
            let head = plan_item
                .and_then(|item| item.provenance.as_ref())
                .map(|provenance| provenance.session_head.clone())
                .unwrap_or_else(|| format!("refs/heads/{}", session.branch));
            (
                "committed work exists only on this checkout or its branch".into(),
                format!(
                    "git log --oneline {}..{head}; deliver it (open a pull request or `aethyme broker submit`) or discard it explicitly with `aethyme broker finish cleanup {id} --force`",
                    short(&target.proof_commit)
                ),
            )
        }
        AuditDisposition::IntegrationOnly => (
            format!(
                "work is on the integration or local branch but not {}, and the cleanup plan does not call it eligible",
                target.proof_ref
            ),
            "aethyme broker advanced integration status; publish integration, then re-run the audit".into(),
        ),
        AuditDisposition::RemoteBranchOnly => (
            format!(
                "work is on a remote branch but not {}, and the cleanup plan could not verify that branch is current",
                target.proof_ref
            ),
            format!(
                "git fetch origin, check the pull request for {}, then re-run the audit",
                session.branch
            ),
        ),
        AuditDisposition::InTarget => (
            format!(
                "work is in {} by content, but the cleanup plan has no recorded proof ({})",
                target.proof_ref,
                plan_item
                    .map(|item| item.disposition.as_str())
                    .unwrap_or("not in the plan: session is not closed")
            ),
            format!(
                "aethyme broker advanced representation scan --session {id}, then representation record with its digest"
            ),
        ),
        AuditDisposition::MissingCheckoutMetadata => (
            "the directory and branch are gone; only the broker session row remains".into(),
            format!("aethyme broker finish cleanup {id}"),
        ),
        AuditDisposition::UnknownProvenance => (
            match (inspection_error, checkout) {
                (Some(error), _) => format!("checkout could not be inspected: {error}"),
                (None, CheckoutState::MissingGitMetadata) => {
                    "Git no longer registers this directory and no branch records its work".into()
                }
                _ => "nothing records what this checkout held".into(),
            },
            format!("inspect {quoted} by hand before any removal"),
        ),
    };
    (Some(blocker), next)
}

fn apply_command(plan: &CleanupPlan) -> String {
    format!(
        "aethyme broker finish cleanup --all-cleaned --apply --confirm {}",
        plan.digest
    )
}

fn summarise(items: &[AuditItem]) -> AuditSummary {
    let mut summary = AuditSummary {
        item_count: items.len(),
        ..AuditSummary::default()
    };
    for disposition in [
        AuditDisposition::Live,
        AuditDisposition::Dirty,
        AuditDisposition::WorktreeOnly,
        AuditDisposition::UnknownProvenance,
        AuditDisposition::RemoteBranchOnly,
        AuditDisposition::IntegrationOnly,
        AuditDisposition::MissingCheckoutMetadata,
        AuditDisposition::InTarget,
    ] {
        summary
            .by_disposition
            .insert(disposition.as_str().to_string(), 0);
    }
    for item in items {
        *summary
            .by_disposition
            .entry(item.disposition.as_str().to_string())
            .or_default() += 1;
        match item.bytes {
            Some(bytes) => {
                summary.measured_bytes += bytes;
                if item.removable {
                    summary.removable_bytes += bytes;
                }
            }
            None => summary.unmeasured_count += 1,
        }
        if item.removable {
            summary.removable_count += 1;
        }
    }
    summary
}
