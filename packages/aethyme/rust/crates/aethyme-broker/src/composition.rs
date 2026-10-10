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
    /// A bounded resolver's accepted proposal, under the named profile
    /// (#665). Never a composition: its content is the resolver's.
    Resolver { profile: String },
}

impl Producer {
    pub fn profile(&self) -> &str {
        match self {
            Self::LegacyGitReplay => LEGACY_GIT_REPLAY_PROFILE,
            Self::Composer { profile } | Self::Resolver { profile } => profile,
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
    /// A bounded resolver wrote the content (#665). Never verified by being
    /// produced: the candidate is checked again as its own candidate.
    Synthesized,
}

impl CompositionMode {
    pub fn code(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Structural => "structural",
            Self::Hybrid => "hybrid",
            Self::Synthesized => "synthesized",
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
///
/// Every candidate is **unverified**, whoever produced it: constructing one
/// proves nothing about behavior, and a resolver's (`Producer::Resolver`)
/// least of all. Nothing here marks a candidate verified, and no field can
/// be set to make it so. Acceptance (L5) must require a passing verification
/// of this exact `subject` before it promotes or publishes anything.
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
    /// A side deleted lines that have an identical twin left in the base,
    /// so a line merge cannot tell which copy went (#664's text profile
    /// limit).
    AmbiguousAnchor,
    /// The result would hold a file where another path needs a directory.
    DirectoryFile,
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
            Self::AmbiguousAnchor => "ambiguous_anchor",
            Self::DirectoryFile => "directory_file",
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
    /// The session's checkout is on another branch than the one recorded,
    /// or moved; submit refuses with `SessionCheckoutDrift`.
    CheckoutDrift,
    /// The session's changes are not its own to submit: unleased paths,
    /// another active session's conflicting lease, or adoption-time foreign
    /// files. Submit refuses with `OwnershipViolation`.
    OwnershipViolation,
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
            Self::CheckoutDrift => "checkout_drift",
            Self::OwnershipViolation => "ownership_violation",
        }
    }
}

/// Inputs the producer cannot represent at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// A pending input commit has more than one parent; the legacy replay
    /// applies only linear history.
    MergeCommit,
    /// The candidate holds an entry a #652 snapshot cannot name (a gitlink,
    /// say), so it has no subject.
    SnapshotEntry,
    /// A pending input commit has no parent at all (a root commit), so it
    /// has no change of its own to apply.
    CommitShape,
    /// A path of the candidate has a checkout-transforming attribute
    /// (`filter`, `working-tree-encoding`, `ident`), which the archive
    /// refuses to retain, so no subject could ever be retained.
    TransformingAttribute,
    /// The repository is a partial clone: the archive reads no source from
    /// one, so the candidate cannot be named.
    PartialClone,
}

impl Unsupported {
    pub fn code(self) -> &'static str {
        match self {
            Self::MergeCommit => "merge_commit",
            Self::SnapshotEntry => "snapshot_entry",
            Self::CommitShape => "commit_shape",
            Self::TransformingAttribute => "transforming_attribute",
            Self::PartialClone => "partial_clone",
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
    CommitOid::parse(text).map_err(BrokerOpError::CandidateSource)
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
        // Submit's preflight, in submit's order: checkout identity, then
        // lease ownership, then the baseline's policy.
        let session_head =
            match crate::merge::require_session_checkout_identity(&session, &checkout, None) {
                Ok(head) => head,
                Err(error @ BrokerOpError::SessionCheckoutDrift { .. }) => {
                    return Ok(CompositionOutcome::Refused {
                        reason: Refusal::CheckoutDrift,
                        detail: error.to_string(),
                    });
                }
                Err(error) => return Err(error),
            };
        let ownership = self.audit_submit_ownership_read_only(session_id)?;
        if !ownership.ok {
            return Ok(CompositionOutcome::Refused {
                reason: Refusal::OwnershipViolation,
                detail: ownership.failure_summary(),
            });
        }
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
            Err(error @ BrokerOpError::UnsupportedSubmissionCommit { parent_count, .. }) => {
                return Ok(CompositionOutcome::Unsupported {
                    reason: if parent_count == 0 {
                        Unsupported::CommitShape
                    } else {
                        Unsupported::MergeCommit
                    },
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
                Err(error) => {
                    let reason = match &error {
                        ArchiveError::UnsupportedEntry { .. }
                        | ArchiveError::InvalidSnapshot(_) => Unsupported::SnapshotEntry,
                        ArchiveError::UnsupportedFilter { .. } => {
                            Unsupported::TransformingAttribute
                        }
                        ArchiveError::PartialClone { .. } => Unsupported::PartialClone,
                        _ => return Err(BrokerOpError::CandidateSource(error)),
                    };
                    return Ok(CompositionOutcome::Unsupported {
                        reason,
                        detail: error.to_string(),
                    });
                }
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
