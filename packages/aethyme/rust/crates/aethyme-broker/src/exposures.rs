use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::{
    AdvisoryResolutionState, Broker, BrokerOpError, CoordinatedCommand, CoordinatedOperation,
    EntryExposureResolutionKind, EntryPathExposure, OperationEffect, OperationProvider,
    ResolvedRemoteTarget,
};

pub const EXPOSURE_RECONCILIATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ExposureRemainingItem {
    pub queue_entry_id: i64,
    pub promotion_sha: String,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AdvisoryReconciliationItem {
    pub advisory_id: i64,
    pub queue_entry_id: i64,
    pub session_id: Option<i64>,
    pub paths: Vec<String>,
    pub blocking_leases: Vec<String>,
    pub eligible: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ExposureReconciliationPlan {
    pub schema_version: u32,
    pub target: ResolvedRemoteTarget,
    pub remote_default_branch_ref: String,
    pub remote_default_branch_sha: String,
    pub tracking_ref: String,
    pub tracking_sha: Option<String>,
    pub tracking_matches_remote: bool,
    pub contained_exposures: Vec<EntryPathExposure>,
    pub remaining_exposures: Vec<ExposureRemainingItem>,
    pub advisories: Vec<AdvisoryReconciliationItem>,
    pub safe: bool,
    pub refusals: Vec<String>,
    pub digest: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ExposureReconciliationApplyReport {
    pub plan: ExposureReconciliationPlan,
    pub verification_operation: CoordinatedOperation,
    pub resolved_exposures: Vec<EntryPathExposure>,
    pub resolved_advisories: Vec<crate::Advisory>,
}

impl Broker {
    pub fn exposure_reconciliation_plan(
        &mut self,
    ) -> Result<ExposureReconciliationPlan, BrokerOpError> {
        let current_branch = self.repo_handle().current_branch()?;
        let remote = self
            .repo_handle()
            .config_get(&format!("branch.{current_branch}.remote"))
            .filter(|remote| remote != ".")
            .unwrap_or_else(|| "origin".into());
        let target = self.repo_handle().resolve_remote_target(&remote, None)?;
        let remote_default = self.repo_handle().remote_default_branch(&remote)?;
        let default_branch = remote_default
            .ref_name
            .strip_prefix("refs/heads/")
            .ok_or_else(|| BrokerOpError::ExposurePlanUnavailable {
                reason: format!(
                    "advertised remote default ref {} is not under refs/heads/",
                    remote_default.ref_name
                ),
            })?;
        let tracking_ref = format!("refs/remotes/{remote}/{default_branch}");
        let tracking_sha = self.repo_handle().resolve_ref(&tracking_ref);
        let tracking_matches_remote = tracking_sha.as_deref() == Some(&remote_default.sha);
        let remote_object_available = self
            .repo_handle()
            .resolve_ref(&remote_default.sha)
            .is_some();
        let mut refusals = Vec::new();
        if !remote_object_available {
            refusals.push(format!(
                "remote default commit {} is not available locally; fetch {} before planning reconciliation",
                remote_default.sha, remote
            ));
        }

        let mut contained_exposures = Vec::new();
        let mut remaining_exposures = Vec::new();
        for exposure in self.store().outstanding_entry_path_exposures()? {
            if remote_object_available
                && self
                    .repo_handle()
                    .is_ancestor(&exposure.promotion_sha, &remote_default.sha)
            {
                contained_exposures.push(exposure);
            } else {
                remaining_exposures.push(ExposureRemainingItem {
                    queue_entry_id: exposure.queue_entry_id,
                    promotion_sha: exposure.promotion_sha,
                    reason: if remote_object_available {
                        "promotion_not_contained_in_remote_default".into()
                    } else {
                        "remote_object_missing".into()
                    },
                });
            }
        }
        contained_exposures.sort_by_key(|exposure| exposure.queue_entry_id);
        remaining_exposures.sort_by_key(|exposure| exposure.queue_entry_id);

        let active_leases = self.store().active_leases()?;
        let mut advisories = Vec::new();
        for advisory in self.store().advisories(true)? {
            if advisory.resolution_state == AdvisoryResolutionState::Resolved {
                continue;
            }
            let Some(queue_entry_id) = advisory.queue_entry_id else {
                continue;
            };
            let Some(exposure) = self.store().entry_path_exposure(queue_entry_id)? else {
                continue;
            };
            if !remote_object_available
                || !self
                    .repo_handle()
                    .is_ancestor(&exposure.promotion_sha, &remote_default.sha)
            {
                continue;
            }
            let mut blocking_leases = active_leases
                .iter()
                .filter(|lease| Some(lease.session_id) == advisory.session_id)
                .filter(|lease| {
                    advisory
                        .paths
                        .iter()
                        .any(|path| crate::leases::paths_overlap(path, &lease.path))
                })
                .map(|lease| lease.path.clone())
                .collect::<Vec<_>>();
            blocking_leases.sort();
            blocking_leases.dedup();
            advisories.push(AdvisoryReconciliationItem {
                advisory_id: advisory.id,
                queue_entry_id,
                session_id: advisory.session_id,
                paths: advisory.paths,
                eligible: blocking_leases.is_empty(),
                blocking_leases,
            });
        }
        advisories.sort_by_key(|advisory| advisory.advisory_id);

        let mut plan = ExposureReconciliationPlan {
            schema_version: EXPOSURE_RECONCILIATION_SCHEMA_VERSION,
            target,
            remote_default_branch_ref: remote_default.ref_name,
            remote_default_branch_sha: remote_default.sha,
            tracking_ref,
            tracking_sha,
            tracking_matches_remote,
            contained_exposures,
            remaining_exposures,
            advisories,
            safe: refusals.is_empty(),
            refusals,
            digest: String::new(),
        };
        plan.digest = exposure_plan_digest(&plan)?;
        Ok(plan)
    }

    pub fn apply_exposure_reconciliation(
        &mut self,
        session_id: i64,
        confirm: &str,
    ) -> Result<ExposureReconciliationApplyReport, BrokerOpError> {
        if confirm.len() != 64 || !confirm.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BrokerOpError::ExposureConfirmationNotSha256);
        }
        let plan = self.exposure_reconciliation_plan()?;
        if plan.digest != confirm {
            return Err(BrokerOpError::ExposureConfirmationMismatch {
                expected: plan.digest,
                actual: confirm.into(),
            });
        }
        if !plan.safe {
            return Err(BrokerOpError::ExposurePlanUnsafe {
                reasons: plan.refusals.join("; "),
            });
        }
        let main_root = self.main_root().to_path_buf();
        let verification = self.run_coordinated_operation_at(
            CoordinatedCommand {
                session_id,
                provider: OperationProvider::Git,
                repository: None,
                resolved_target: Some(plan.target.clone()),
                scope: Some(format!(
                    "exposures:verify:{}",
                    plan.remote_default_branch_ref
                )),
                declared_effect: Some(OperationEffect::Read),
                destructive_confirmed: false,
                authorization_reason: None,
                args: vec![
                    "ls-remote".into(),
                    plan.target.remote_name.clone(),
                    plan.remote_default_branch_ref.clone(),
                ],
            },
            &main_root,
        )?;
        if !verification.ok() {
            return Err(BrokerOpError::ExposureVerificationFailed {
                operation_id: verification.operation.id,
                status: verification.operation.status.as_str(),
            });
        }
        let verified_sha = verification
            .stdout
            .lines()
            .find_map(|line| line.split_once('\t'))
            .filter(|(_, name)| *name == plan.remote_default_branch_ref)
            .map(|(sha, _)| sha.to_string())
            .unwrap_or_else(|| "<missing>".into());
        if verified_sha != plan.remote_default_branch_sha {
            return Err(BrokerOpError::ExposureRemoteMoved {
                expected: plan.remote_default_branch_sha.clone(),
                actual: verified_sha,
            });
        }

        let contained_ids = plan
            .contained_exposures
            .iter()
            .map(|exposure| exposure.queue_entry_id)
            .collect::<Vec<_>>();
        let advisory_entry_ids = plan
            .advisories
            .iter()
            .filter(|advisory| advisory.eligible)
            .map(|advisory| advisory.queue_entry_id)
            .collect::<BTreeSet<_>>();
        let evidence = format!(
            "broker exposure reconciliation verified remote {} at {} via operation {}",
            plan.remote_default_branch_ref,
            plan.remote_default_branch_sha,
            verification.operation.id
        );
        let resolved_exposures = self.store().resolve_entry_path_exposures(
            &contained_ids,
            EntryExposureResolutionKind::ExternalReconciliation,
            &plan.remote_default_branch_sha,
            &evidence,
        )?;
        let resolved_advisories = self
            .store()
            .resolve_entry_advisories_without_active_leases(
                &advisory_entry_ids.into_iter().collect::<Vec<_>>(),
                &evidence,
            )?;
        let _ = self.refresh_advisory_projection();
        Ok(ExposureReconciliationApplyReport {
            plan,
            verification_operation: verification.operation,
            resolved_exposures,
            resolved_advisories,
        })
    }
}

fn exposure_plan_digest(plan: &ExposureReconciliationPlan) -> Result<String, BrokerOpError> {
    let bytes = serde_json::to_vec(plan)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
