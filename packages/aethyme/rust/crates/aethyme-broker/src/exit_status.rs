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
        | E::ReuseSyncNotFastForward { .. } => REFUSED,
        _ => FAILED,
    }
}

/// The exit code for a finished submission. Only a conflict or a rejection
/// is non-zero; a queued or verify-only outcome is the command succeeding.
pub fn for_submission(status: MergeStatus) -> u8 {
    match status {
        MergeStatus::Conflict => REFUSED,
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
    fn a_submission_is_non_zero_only_when_it_did_not_pass() {
        assert_eq!(for_submission(MergeStatus::Promoted), SUCCESS);
        assert_eq!(for_submission(MergeStatus::Verified), SUCCESS);
        assert_eq!(for_submission(MergeStatus::Conflict), REFUSED);
        assert_eq!(for_submission(MergeStatus::Rejected), VERIFICATION_FAILED);
    }
}
