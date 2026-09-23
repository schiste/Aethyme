//! Process exit codes for `aethyme broker`.
//!
//! Agents drive the broker and branch on how a command ended. A single
//! "exit 1 for everything" made a policy refusal, a failed gate, and a remote
//! write whose outcome is unknown look alike, and `submit --json` exited 0 even
//! when it rejected. Each code below names one outcome a caller acts on
//! differently; anything not classified stays `FAILED` (1), as before.

use crate::{BrokerOpError, MergeStatus};

/// The command did what it was asked.
pub const SUCCESS: u8 = 0;
/// An unclassified failure: the pre-existing meaning of a non-zero exit.
pub const FAILED: u8 = 1;
/// The command line itself was wrong; see `--help`.
pub const USAGE: u8 = 2;
/// A policy, lease, confirmation or state precondition refused the request.
/// Nothing was changed; fix the precondition, then retry.
pub const REFUSED: u8 = 3;
/// Verification ran and did not pass (a gate or graph integrity check).
pub const VERIFICATION_FAILED: u8 = 4;
/// A remote write may or may not have happened. Inspect external state and
/// reconcile; never retry blindly.
pub const OUTCOME_UNKNOWN: u8 = 5;
/// The host could not run the command: a missing tool, path or remote base,
/// or an I/O failure outside the broker's control.
pub const ENVIRONMENT: u8 = 6;

/// The exit code for a broker error, by what the caller should do next.
pub fn for_broker_error(error: &BrokerOpError) -> u8 {
    use BrokerOpError as E;
    match error {
        E::CoordinatedOperationTimedOut { .. } => OUTCOME_UNKNOWN,
        // A git killed at its deadline judged nothing: the host, a remote or
        // a lock is wedged, so the next step is to look at the environment.
        E::Git(crate::GitError::TimedOut { .. }) => ENVIRONMENT,
        E::GraphIntegrityRejected { .. }
        | E::NotVerified { .. }
        | E::ExposureVerificationFailed { .. }
        | E::ShipVerificationMismatch { .. } => VERIFICATION_FAILED,
        E::Spawn { .. }
        | E::OperationSpawn { .. }
        | E::OperationIo { .. }
        | E::AdvisoryProjectionIo { .. }
        | E::WorktreeRootUnavailable { .. }
        | E::StartBaseUnavailable { .. }
        | E::UpstreamRefNotFound { .. }
        | E::RepresentationUnavailable { .. }
        | E::MainReconcileUnavailable { .. }
        | E::ExposurePlanUnavailable { .. }
        | E::ShipPlanUnavailable { .. }
        | E::ShipDeliveryUnavailable { .. }
        | E::ShipRemoteBaseUnavailable { .. } => ENVIRONMENT,
        E::MainReconcileUnsafe { .. }
        | E::MainReconcileConfirmationMismatch { .. }
        | E::DuplicatePendingOperation { .. }
        | E::CoordinatedLockBusy { .. }
        | E::AdmissionTimedOut { .. }
        | E::DirtyWorktree { .. }
        | E::CleanupConfirmationNotSha256
        | E::CleanupConfirmationMismatch { .. }
        | E::GcConfirmationNotSha256
        | E::GcConfirmationMismatch { .. }
        | E::GcResumeConfirmationMismatch { .. }
        | E::PromotionRecordConfirmationMismatch { .. }
        | E::GcLocked { .. }
        | E::UnsafeRepairPlan { .. }
        | E::RepairNotApplicable { .. }
        | E::UnsafeCheckpointRecovery { .. }
        | E::CheckpointConfirmationNotSha256
        | E::CheckpointConfirmationMismatch { .. }
        | E::CheckpointPreservationRefConflict { .. }
        | E::InvalidReconciliationResolution { .. }
        | E::ReconciliationRecoveryRequired { .. }
        | E::ReviewRequiresPullRequestHead { .. }
        | E::NestedWorktreePath { .. }
        | E::RepositoryContract { .. }
        | E::LeaseClaimConflict { .. }
        | E::InvalidLeasePath { .. }
        | E::OwnershipViolation { .. }
        | E::InvalidCoordinatedOperation { .. }
        | E::ClosedSessionOperation { .. }
        | E::CoordinatedOperationBlocked { .. }
        | E::UnsafeSubmissionPlan { .. }
        | E::UnsupportedSubmissionCommit { .. }
        | E::ExposurePlanUnsafe { .. }
        | E::ExposureConfirmationNotSha256
        | E::ExposureConfirmationMismatch { .. }
        | E::ExposureRemoteMoved { .. }
        | E::ShipEntryNotPromoted { .. }
        | E::ShipEntryNotOnIntegration { .. }
        | E::ShipPublicationPolicy { .. }
        | E::ShipDeliveryOverrideUnsafe { .. }
        | E::ShipDeliveryRequiresExplicitSelection { .. }
        | E::ShipDeliveryPlanDigestRequired
        | E::ShipDeliveryPlanDigestNotSha256
        | E::ShipDeliveryPlanDigestMismatch { .. }
        | E::ShipDeliveryBranchConflict { .. }
        | E::ShipDeliveryPullRequestMismatch { .. }
        | E::ShipConfirmationNotFullSha
        | E::ShipConfirmationMismatch { .. }
        | E::ReconciliationConfirmationRequired { .. }
        | E::ReconciliationConfirmationNotSha256
        | E::ReconciliationConfirmationMismatch { .. }
        | E::ShipRemoteMoved { .. }
        | E::ShipNonFastForward { .. }
        | E::ShipLocalMainUnsafe { .. }
        | E::SessionExistsForWorktree { .. }
        | E::ReuseSyncRequiresReuse
        | E::ReuseSyncDirty { .. }
        | E::ReuseSyncNotFastForward { .. }
        | E::GatePolicyUntrusted { .. } => REFUSED,
        _ => FAILED,
    }
}

/// Whether every failing gate failed because the host could not run it
/// (low disk, lock or resource contention, environment) rather than because
/// the code failed. An empty set is not: a graph-integrity rejection has no
/// gate outcomes and did judge the tree.
pub fn failures_are_environmental<'a>(classes: impl IntoIterator<Item = Option<&'a str>>) -> bool {
    let mut any = false;
    for class in classes {
        any = true;
        if !matches!(class, Some("resource_contention" | "environment")) {
            return false;
        }
    }
    any
}

/// The exit code for a finished submission. Only a conflict or a rejection
/// is non-zero; a queued or verify-only outcome is the command succeeding.
/// A rejection whose failing gates never ran for host reasons is ENVIRONMENT,
/// not VERIFICATION_FAILED: the code was not judged, so "fix the code" is the
/// wrong next step.
pub fn for_submission(status: MergeStatus, gates: &[crate::gates::GateRunOutcome]) -> u8 {
    match status {
        MergeStatus::Conflict => REFUSED,
        MergeStatus::Rejected
            if failures_are_environmental(
                gates
                    .iter()
                    .filter(|gate| gate.status != crate::GateStatus::Pass)
                    .map(|gate| gate.failure_class.map(|class| class.as_str())),
            ) =>
        {
            ENVIRONMENT
        }
        MergeStatus::Rejected => VERIFICATION_FAILED,
        _ => SUCCESS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_distinct() {
        let codes = [
            SUCCESS,
            FAILED,
            USAGE,
            REFUSED,
            VERIFICATION_FAILED,
            OUTCOME_UNKNOWN,
            ENVIRONMENT,
        ];
        let unique: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len());
    }

    #[test]
    fn a_timed_out_git_is_an_environment_failure_and_other_git_errors_are_not() {
        let timed_out = BrokerOpError::Git(crate::GitError::TimedOut {
            args: "fetch origin".to_string(),
            seconds: 600,
        });
        assert_eq!(for_broker_error(&timed_out), ENVIRONMENT);
        let failed = BrokerOpError::Git(crate::GitError::Git {
            args: "fetch origin".to_string(),
            stderr: "fatal: bad".to_string(),
        });
        assert_eq!(for_broker_error(&failed), FAILED);
    }

    #[test]
    fn a_submission_is_non_zero_only_when_it_did_not_pass() {
        assert_eq!(for_submission(MergeStatus::Promoted, &[]), SUCCESS);
        assert_eq!(for_submission(MergeStatus::Verified, &[]), SUCCESS);
        assert_eq!(for_submission(MergeStatus::Conflict, &[]), REFUSED);
        // A graph-integrity rejection has no gate outcomes and did judge the tree.
        assert_eq!(
            for_submission(MergeStatus::Rejected, &[]),
            VERIFICATION_FAILED
        );
    }

    #[test]
    fn only_host_failures_count_as_environmental() {
        assert!(failures_are_environmental([Some("resource_contention")]));
        assert!(failures_are_environmental([
            Some("environment"),
            Some("resource_contention")
        ]));
        assert!(!failures_are_environmental([
            Some("resource_contention"),
            Some("test_failure")
        ]));
        assert!(!failures_are_environmental([None]));
        assert!(!failures_are_environmental(
            std::iter::empty::<Option<&str>>()
        ));
    }
}
