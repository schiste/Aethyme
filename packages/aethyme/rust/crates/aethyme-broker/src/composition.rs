//! The candidate boundary (L4, #663; plan v3 §6.1, §6.6, §7.3).
//!
//! Constructing a candidate is one operation; verifying, gating and promoting
//! it are others. Everything that produces a candidate, the legacy Git replay
//! here and the composer of #664 later, returns a [`CompositionOutcome`]. Only
//! [`CompositionOutcome::Candidate`] names source: every other outcome carries
//! no snapshot, tree or commit, so nothing success-shaped can be read out of a
//! conflict or a refusal.
//!
//! [`Broker::legacy_candidate`] builds, for one session, the candidate that
//! `submit` would gate on now, and changes nothing: no queue row, gate, slot,
//! ref, trust record or event. Like `submission_plan`, replaying can leave
//! unreachable Git objects behind. Submit itself does not use this boundary
//! yet: routing the gate through it waits for the typed verification
//! candidate of #692 (PR #725). See
//! `docs/architecture/local-v3-l4-boundary.md`.
//!
//! Provisional until E1 (#650) closes D04–D06/D12: the outcome shape is
//! internal and may change with the first composer profile.

use std::path::Path;

use aethyme_contracts::experimental_v0::SourceSnapshotId;

use crate::collaboration_archive::{self, ArchiveError, CommitOid};
use crate::merge::{
    PromoteConfig, PromoteMode, SubmissionCommitOwnership, SubmissionIntegrationState,
    VERIFIED_AGAINST_INTEGRATION, VERIFIED_AGAINST_UPSTREAM,
};
use crate::{Broker, BrokerOpError};

/// The profile ID of candidates the legacy Git replay produces.
pub const LEGACY_GIT_REPLAY_PROFILE: &str = "legacy-git-replay/v0";

/// What built a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Producer {
    /// `submit`'s replay of the session's pending commits, one
    /// `git merge-tree` per commit.
    LegacyGitReplay,
    /// A composer running the named profile (#664).
    Composer { profile: String },
}

impl Producer {
    pub fn profile(&self) -> &str {
        match self {
            Self::LegacyGitReplay => LEGACY_GIT_REPLAY_PROFILE,
            Self::Composer { profile } => profile,
        }
    }
}

/// How the candidate's content was actually merged, as observed, not as
/// hoped. A producer that cannot tell whether a structural engine fell back
/// to text reports `Hybrid` (plan §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositionMode {
    Text,
    Structural,
    Hybrid,
}

impl CompositionMode {
    pub fn code(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Structural => "structural",
            Self::Hybrid => "hybrid",
        }
    }
}

/// One contribution a candidate applies: its own change from `base` to
/// `result`. Snapshot IDs are filled in when the producer read them from the
/// archive (#657); the legacy replay knows only commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateInput {
    pub base: CommitOid,
    pub result: CommitOid,
    pub base_snapshot: Option<SourceSnapshotId>,
    pub result_snapshot: Option<SourceSnapshotId>,
}

/// A complete candidate: the exact source the gates would judge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The #652 identity of the candidate's committed bytes.
    pub subject: SourceSnapshotId,
    /// The Git tree the candidate materializes as.
    pub tree: String,
    /// A commit of `tree` whose only parent is `baseline`. Nothing refers to
    /// it; acceptance is a separate step that decides whether anything will.
    pub commit: CommitOid,
    /// The accepted state the inputs were applied to.
    pub baseline: CommitOid,
    /// Where `baseline` came from: `integration` or `upstream`, as in a
    /// submit outcome's `verified_against.source`, or `retained` for a
    /// composer's baseline read from the archive.
    pub baseline_source: &'static str,
    /// In application order.
    pub inputs: Vec<CandidateInput>,
    pub producer: Producer,
    pub mode: CompositionMode,
}

/// One path an input could not be applied to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionConflict {
    pub path: String,
    /// The input whose application conflicted.
    pub input: CommitOid,
    pub reason: ConflictReason,
}

/// Why a path conflicted. The legacy replay reports only `content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictReason {
    /// Both sides changed overlapping content.
    Content,
    /// One side deleted the path, the other changed it.
    DeleteModify,
    /// Both sides added the path with different content.
    AddAdd,
    /// Both sides changed the path's kind (mode) differently.
    Mode,
    /// Both sides changed a binary file or symlink: exact replacement only.
    Binary,
    /// A side moved a block of lines, which a line merge cannot carry
    /// another side's edit along (#664's text profile limit).
    MovedBlock,
}

impl ConflictReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::DeleteModify => "delete_modify",
            Self::AddAdd => "add_add",
            Self::Mode => "mode",
            Self::Binary => "binary",
            Self::MovedBlock => "moved_block",
        }
    }
}

/// Why construction was refused before any candidate could exist. The
/// composer's codes are reserved here so every producer shares one
/// vocabulary (plan §7.2, §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// An input's base is neither the baseline nor explained by known
    /// lineage.
    UnknownBase,
    DependencyCycle,
    MissingInput,
    CompetingRevisions,
    BudgetExhausted,
    /// The selection cannot be separated from a synthesized result.
    InseparableSelection,
    /// Legacy commit provenance is ambiguous; submit refuses the same plan.
    UnsafePlan,
    /// The gate policy at the baseline is not trusted on this machine.
    UntrustedPolicy,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownBase => "unknown_base",
            Self::DependencyCycle => "dependency_cycle",
            Self::MissingInput => "missing_input",
            Self::CompetingRevisions => "competing_revisions",
            Self::BudgetExhausted => "budget_exhausted",
            Self::InseparableSelection => "inseparable_selection",
            Self::UnsafePlan => "unsafe_plan",
            Self::UntrustedPolicy => "untrusted_policy",
        }
    }
}

/// Inputs the producer cannot represent at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// A pending input commit has other than one parent; the legacy replay
    /// applies only linear history.
    MergeCommit,
    /// The candidate holds an entry a #652 snapshot cannot name (a gitlink,
    /// say), so it has no subject.
    SnapshotEntry,
}

impl Unsupported {
    pub fn code(self) -> &'static str {
        match self {
            Self::MergeCommit => "merge_commit",
            Self::SnapshotEntry => "snapshot_entry",
        }
    }
}

/// The result of constructing one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositionOutcome {
    Candidate(Candidate),
    /// Applying every input leaves the baseline's content unchanged: there is
    /// nothing to verify.
    NoChange {
        baseline: CommitOid,
    },
    Conflict {
        baseline: CommitOid,
        conflicts: Vec<CompositionConflict>,
    },
    Unsupported {
        reason: Unsupported,
        detail: String,
    },
    Refused {
        reason: Refusal,
        detail: String,
    },
}

impl CompositionOutcome {
    /// `candidate`, `no_change`, `conflict`, or the unsupported or refusal
    /// reason's own code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Candidate(_) => "candidate",
            Self::NoChange { .. } => "no_change",
            Self::Conflict { .. } => "conflict",
            Self::Unsupported { reason, .. } => reason.code(),
            Self::Refused { reason, .. } => reason.code(),
        }
    }

    pub fn candidate(&self) -> Option<&Candidate> {
        match self {
            Self::Candidate(candidate) => Some(candidate),
            _ => None,
        }
    }
}

fn commit_oid(text: &str) -> Result<CommitOid, BrokerOpError> {
    CommitOid::parse(text).map_err(archive_failure)
}

fn archive_failure(error: ArchiveError) -> BrokerOpError {
    BrokerOpError::Store(crate::BrokerError::Io {
        path: "<candidate snapshot>".into(),
        source: std::io::Error::other(format!("{}: {error}", error.code())),
    })
}

impl Broker {
    /// Build the candidate `submit` would gate on for `session_id`, without
    /// queueing, gating, promoting, moving a ref or recording trust.
    ///
    /// The baseline is chosen as submit chooses it: the fetched default
    /// branch under `verify-only`, otherwise the integration branch after the
    /// follows-main refresh submit would perform (computed, not performed).
    pub fn legacy_candidate(&self, session_id: i64) -> Result<CompositionOutcome, BrokerOpError> {
        let session = self.store_ref().session(session_id)?;
        let checkout = crate::git::GitRepo::discover(Path::new(&session.worktree_path))?;
        let session_head = checkout.head_commit()?;
        let (baseline_source, base) = self.candidate_baseline()?;

        let policy = crate::broker::gate_trust::policy_at_commit(self.repo_handle(), &base)?;
        match crate::broker::gate_trust::assess_trusted(
            &self.main_root_path(),
            &policy,
            Some(self.store_ref()),
        ) {
            Ok(_) => {}
            Err(error @ BrokerOpError::GatePolicyUntrusted { .. }) => {
                return Ok(CompositionOutcome::Refused {
                    reason: Refusal::UntrustedPolicy,
                    detail: error.to_string(),
                });
            }
            Err(error) => return Err(error),
        }

        let plan = self.build_submission_plan(&session, &session_head, &base)?;
        let replay = match self.replay_submission_plan(&plan) {
            Ok(replay) => replay,
            Err(BrokerOpError::UnsafeSubmissionPlan { reason, .. }) => {
                return Ok(CompositionOutcome::Refused {
                    reason: Refusal::UnsafePlan,
                    detail: reason,
                });
            }
            Err(error @ BrokerOpError::UnsupportedSubmissionCommit { .. }) => {
                return Ok(CompositionOutcome::Unsupported {
                    reason: Unsupported::MergeCommit,
                    detail: error.to_string(),
                });
            }
            Err(error) => return Err(error),
        };
        let baseline = commit_oid(&base)?;
        if !replay.conflicts.is_empty() {
            let conflicts = replay
                .conflict_details
                .iter()
                .map(|detail| {
                    Ok(CompositionConflict {
                        path: detail.path.clone(),
                        input: commit_oid(&detail.originating_commit)?,
                        reason: ConflictReason::Content,
                    })
                })
                .collect::<Result<_, BrokerOpError>>()?;
            return Ok(CompositionOutcome::Conflict {
                baseline,
                conflicts,
            });
        }

        let repo = self.repo_handle();
        // Submit gates a primary-checkout session even when its tree equals
        // the base: the follows-main refresh already moved integration onto
        // its HEAD, and that movement is what gets verified.
        let main_checkout_session = Path::new(&session.worktree_path) == self.main_root_path();
        if replay.tree == repo.commit_tree_id(&base)? && !main_checkout_session {
            return Ok(CompositionOutcome::NoChange { baseline });
        }

        let inputs = plan
            .commits
            .iter()
            .filter(|commit| {
                commit.ownership == SubmissionCommitOwnership::SessionOwned
                    && commit.integration_state == SubmissionIntegrationState::Pending
            })
            .map(|commit| {
                Ok(CandidateInput {
                    base: commit_oid(&commit.parents[0])?,
                    result: commit_oid(&commit.commit)?,
                    base_snapshot: None,
                    result_snapshot: None,
                })
            })
            .collect::<Result<Vec<_>, BrokerOpError>>()?;
        let commit = commit_oid(&repo.commit_tree(
            &replay.tree,
            &[&base],
            &format!("chore(broker): candidate for session {session_id}"),
            &crate::attribution::Attribution::broker_only(),
        )?)?;
        let subject =
            match collaboration_archive::snapshot_of_commit(&self.main_root_path(), &commit) {
                Ok(snapshot) => snapshot.id(),
                Err(error @ ArchiveError::UnsupportedEntry { .. })
                | Err(error @ ArchiveError::InvalidSnapshot(_)) => {
                    return Ok(CompositionOutcome::Unsupported {
                        reason: Unsupported::SnapshotEntry,
                        detail: error.to_string(),
                    });
                }
                Err(error) => return Err(archive_failure(error)),
            };
        Ok(CompositionOutcome::Candidate(Candidate {
            subject,
            tree: replay.tree,
            commit,
            baseline,
            baseline_source,
            inputs,
            producer: Producer::LegacyGitReplay,
            mode: CompositionMode::Text,
        }))
    }

    /// [`Broker::submission_base`] without its side effects: where the
    /// integration branch is missing or behind the main checkout, submit
    /// would create or fast-forward it first, so the base is the main
    /// checkout's HEAD.
    fn candidate_baseline(&self) -> Result<(&'static str, String), BrokerOpError> {
        let config = PromoteConfig::load(&self.main_root_path());
        let repo = self.repo_handle();
        if config.mode == PromoteMode::VerifyOnly
            && let Some((_, commit)) = repo.upstream_default()
        {
            return Ok((VERIFIED_AGAINST_UPSTREAM, commit));
        }
        let head = repo.head_commit()?;
        let commit = match repo.resolve_ref(&config.branch) {
            Some(commit) if commit == head || !repo.is_ancestor(&commit, &head) => commit,
            _ => head,
        };
        Ok((VERIFIED_AGAINST_INTEGRATION, commit))
    }
}
