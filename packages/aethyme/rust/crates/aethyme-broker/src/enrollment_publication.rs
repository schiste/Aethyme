//! Publication of a verified first-enrollment commit.
//!
//! `aethyme deploy execute` lands the commit that enrolls a repository with a
//! narrow, coordinated, non-force push of that exact commit to the remote
//! default branch. It used to get there through promotion: submit, promote
//! onto the integration branch, then plan and publish the promoted prefix. The
//! commit is now taken straight from its verified queue entry, so enrollment
//! neither creates a promoted entry nor reads, creates or moves the
//! integration branch.
//!
//! What the push keeps from that path:
//! - the queue entry must vouch for the exact commit: same session, same
//!   head, a verified status, and a gated tree identical to the commit's tree;
//! - the commit must be a single commit on top of the reviewed remote base:
//!   exactly one parent, and that parent is the base. Otherwise a fast-forward
//!   push could carry commits the plan never reviewed (the old path refused
//!   unrecorded commits between the remote tip and the publication);
//! - the `[publication]` policy committed on the reviewed remote base is
//!   honoured, and a `review_gated` policy refuses before anything is fetched;
//! - remote freshness is refreshed through the operation coordinator, and the
//!   push happens only from the reviewed base (or as an idempotent retry of
//!   the same commit) and only as a fast-forward;
//! - an unknown push outcome blocks the repository through the coordinator
//!   instead of being retried;
//! - the remote is read back afterward and must name the commit;
//! - the primary checkout is fast-forwarded only when it is the clean,
//!   unchanged default branch, and is revalidated after the push.

use crate::broker::{Broker, BrokerOpError};
use crate::operations::CoordinatedCommand;
use crate::ship::{
    ShipPublicationMode, assess_local_main_sync_repo, ls_remote_sha, publication_policy_at,
    ship_operation_failure,
};
use crate::types::{MergeQueueEntry, MergeStatus, OperationEffect, OperationProvider};

/// What the caller has confirmed: one verified commit and the remote base it
/// was reviewed against.
#[derive(Debug, Clone)]
pub struct EnrollmentPublicationRequest<'a> {
    /// Queue entry whose verification vouches for `commit`.
    pub queue_entry_id: i64,
    /// Session that must own that queue entry.
    pub session_id: i64,
    /// Exact commit to publish (full SHA).
    pub commit: &'a str,
    /// Remote name, such as `origin`.
    pub remote: &'a str,
    /// Remote default branch, such as `refs/heads/main`.
    pub remote_ref: &'a str,
    /// Remote tip the publication was reviewed against.
    pub planned_base: &'a str,
    /// Why this write is authorized, recorded on each coordinated operation.
    pub authorization: &'a str,
}

/// Remote evidence of a completed enrollment publication.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnrollmentPublication {
    pub published_sha: String,
    pub verified_remote_sha: String,
    pub push_operation_id: i64,
    pub local_main_synchronized: bool,
}

impl Broker {
    /// The queue entry `entry_id` when its verification vouches for exactly
    /// `commit`: it exists, belongs to `session_id`, is verified (or was
    /// promoted by an older binary), records `commit` as its head, and its
    /// gated tree is `commit`'s tree. `None` when any of that does not hold;
    /// nothing is changed either way.
    pub fn entry_vouching_for_commit(
        &mut self,
        entry_id: i64,
        session_id: i64,
        commit: &str,
    ) -> Result<Option<MergeQueueEntry>, BrokerOpError> {
        let Some(entry) = self
            .store()
            .merge_queue()?
            .into_iter()
            .find(|entry| entry.id == entry_id)
        else {
            return Ok(None);
        };
        if !matches!(entry.status, MergeStatus::Verified | MergeStatus::Promoted)
            || entry.session_id != session_id
            || entry.head_commit != commit
        {
            return Ok(None);
        }
        let tree = self.repo_handle().commit_tree_id(commit)?;
        Ok((entry.merged_tree.as_deref() == Some(tree.as_str())).then_some(entry))
    }

    /// Push one verified enrollment commit to the remote default branch and
    /// confirm it there. See the module documentation for the guarantees.
    pub fn publish_enrollment_commit(
        &mut self,
        request: &EnrollmentPublicationRequest<'_>,
    ) -> Result<EnrollmentPublication, BrokerOpError> {
        let _span = crate::phase_telemetry::span("enrollment_publication");
        let commit = request.commit;
        if commit.len() != 40
            || !commit
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(BrokerOpError::ShipConfirmationNotFullSha);
        }
        let entry = self
            .entry_vouching_for_commit(request.queue_entry_id, request.session_id, commit)?
            .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                what: "verified enrollment commit",
                reason: format!(
                    "queue entry {} does not vouch for {commit}; verify the enrollment session again",
                    request.queue_entry_id
                ),
            })?;

        // Exactly the reviewed base plus this one commit may be published.
        let parents = self.repo_handle().commit_parents(commit)?;
        if parents.as_slice() != [request.planned_base.to_string()] {
            return Err(BrokerOpError::ShipPlanUnavailable {
                what: "reviewed enrollment base",
                reason: format!(
                    "enrollment commit {commit} has parent(s) [{}], not exactly the reviewed remote base {}; \
                     publishing it would carry unreviewed commits. Nothing was fetched or pushed; \
                     regenerate `aethyme deploy plan --repo .`",
                    parents.join(", "),
                    request.planned_base
                ),
            });
        }

        // The policy committed on the reviewed remote base decides, never the
        // candidate: an enrollment commit cannot authorize its own push.
        let policy = publication_policy_at(self, request.planned_base)?;
        if policy.mode != ShipPublicationMode::Direct {
            return Err(BrokerOpError::ShipPublicationPolicy {
                reason: "first enrollment publishes directly, and the remote default branch's \
                         [publication] policy is review_gated"
                    .into(),
                remediation: "land the enrollment through a reviewed pull request instead: commit \
                              the changes `aethyme deploy plan --repo . --diff` shows on a branch \
                              and open a pull request, or commit `[publication] mode = \"direct\"` \
                              on the default branch and plan again"
                    .into(),
            });
        }

        let target = self
            .repo_handle()
            .resolve_remote_target(request.remote, None)
            .map_err(|error| BrokerOpError::ShipPlanUnavailable {
                what: "remote target",
                reason: error.to_string(),
            })?;
        let default_branch = request
            .remote_ref
            .strip_prefix("refs/heads/")
            .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                what: "remote default branch",
                reason: format!("{} is not under refs/heads/", request.remote_ref),
            })?;
        let tracking_ref = format!("refs/remotes/{}/{default_branch}", target.remote_name);
        let local_default_ref = format!("refs/heads/{default_branch}");
        let local_default_sha = self.repo_handle().resolve_ref(&local_default_ref);
        let sync_main = match &local_default_sha {
            Some(expected) => self.local_main_clean_for(default_branch, expected, commit)?,
            None => false,
        };
        let main_root = self.main_root().to_path_buf();
        let wait = crate::QueueWait::Forever;
        let command =
            |scope: &str, effect: Option<OperationEffect>, args: Vec<String>| CoordinatedCommand {
                session_id: entry.session_id,
                provider: OperationProvider::Git,
                repository: None,
                resolved_target: Some(target.clone()),
                scope: Some(format!("enrollment:{scope}:{}", request.remote_ref)),
                declared_effect: effect,
                destructive_confirmed: false,
                cross_session: None,
                ref_write_acknowledged: false,
                authorization_reason: (effect != Some(OperationEffect::Read))
                    .then(|| format!("{}; direct publication policy", request.authorization)),
                args,
            };

        let fetch = self.run_coordinated_operation_at_with_wait(
            command(
                "fetch",
                None,
                vec![
                    "fetch".into(),
                    "--no-tags".into(),
                    "--force".into(),
                    target.remote_name.clone(),
                    format!("{}:{tracking_ref}", request.remote_ref),
                ],
            ),
            &main_root,
            wait,
        )?;
        if !fetch.ok() {
            return Err(ship_operation_failure("fetch", &fetch));
        }
        let fetched = self
            .repo_handle()
            .resolve_ref(&tracking_ref)
            .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                what: "fetched remote default branch",
                reason: format!("{tracking_ref} does not resolve after fetch"),
            })?;
        // The reviewed base, or this exact commit on an idempotent retry.
        if fetched != request.planned_base && fetched != commit {
            return Err(BrokerOpError::ShipRemoteMoved {
                remote_ref: request.remote_ref.into(),
                expected: request.planned_base.into(),
                actual: fetched,
            });
        }
        if !self.repo_handle().is_ancestor(&fetched, commit) {
            return Err(BrokerOpError::ShipNonFastForward {
                remote_ref: request.remote_ref.into(),
                remote_sha: fetched,
                integration_sha: commit.into(),
            });
        }

        let push = self.run_coordinated_operation_at_with_wait(
            command(
                "push",
                None,
                vec![
                    "push".into(),
                    target.remote_name.clone(),
                    format!("{commit}:{}", request.remote_ref),
                ],
            ),
            &main_root,
            wait,
        )?;
        if !push.ok() {
            return Err(ship_operation_failure("push", &push));
        }

        let verify = self.run_coordinated_operation_at_with_wait(
            command(
                "verify",
                Some(OperationEffect::Read),
                vec![
                    "ls-remote".into(),
                    target.remote_name.clone(),
                    request.remote_ref.into(),
                ],
            ),
            &main_root,
            wait,
        )?;
        if !verify.ok() {
            return Err(ship_operation_failure("verification", &verify));
        }
        if verify.stdout_truncated {
            return Err(BrokerOpError::ShipDeliveryUnavailable {
                reason: "remote reference verification stdout was truncated; refusing incomplete evidence".into(),
            });
        }
        let verified_remote_sha =
            ls_remote_sha(&verify.stdout, request.remote_ref).unwrap_or_else(|| "<missing>".into());
        if verified_remote_sha != commit {
            self.store().transition_coordinated_operation(
                verify.operation.id,
                crate::OperationStatus::Failed,
                Some(1),
                Some(
                    &serde_json::json!({
                        "reason": "remote_verification_mismatch",
                        "expected": commit,
                        "actual": verified_remote_sha,
                    })
                    .to_string(),
                ),
            )?;
            return Err(BrokerOpError::ShipVerificationMismatch {
                remote_ref: request.remote_ref.into(),
                expected: commit.into(),
                actual: verified_remote_sha,
            });
        }

        let mut local_main_synchronized = false;
        if sync_main && let Some(expected) = local_default_sha.as_deref() {
            if !self.local_main_clean_for(default_branch, expected, commit)? {
                return Err(BrokerOpError::ShipLocalMainMovedAfterPublish {
                    published_sha: commit.into(),
                    reason: format!(
                        "local {local_default_ref} or the primary checkout changed during publication"
                    ),
                });
            }
            let mut sync = command(
                "sync",
                None,
                vec!["merge".into(), "--ff-only".into(), commit.into()],
            );
            sync.resolved_target = None;
            sync.scope = Some(format!("enrollment:sync:{local_default_ref}"));
            let sync = self.run_coordinated_operation_at_with_wait(sync, &main_root, wait)?;
            if !sync.ok() {
                return Err(ship_operation_failure("local-main synchronization", &sync));
            }
            let after = self.repo_handle().head_commit()?;
            if after != commit {
                self.store().transition_coordinated_operation(
                    sync.operation.id,
                    crate::OperationStatus::Failed,
                    Some(1),
                    Some(
                        &serde_json::json!({
                            "reason": "local_main_verification_mismatch",
                            "expected": commit,
                            "actual": after,
                        })
                        .to_string(),
                    ),
                )?;
                return Err(BrokerOpError::ShipLocalMainMovedAfterPublish {
                    published_sha: commit.into(),
                    reason: format!("expected local main {commit}, observed {after}"),
                });
            }
            local_main_synchronized = true;
        }

        Ok(EnrollmentPublication {
            published_sha: commit.into(),
            verified_remote_sha,
            push_operation_id: push.operation.id,
            local_main_synchronized,
        })
    }

    /// Whether the primary checkout is the unchanged, fully clean default
    /// branch, so fast-forwarding it to `commit` can lose nothing.
    fn local_main_clean_for(
        &self,
        default_branch: &str,
        expected_head: &str,
        commit: &str,
    ) -> Result<bool, BrokerOpError> {
        let assessment = assess_local_main_sync_repo(
            self.repo_handle(),
            default_branch,
            &format!("refs/heads/{default_branch}"),
            expected_head,
            commit,
        )
        .map_err(|reason| BrokerOpError::ShipPlanUnavailable {
            what: "local-main synchronization",
            reason,
        })?;
        Ok(assessment.safe
            && assessment.tracked_dirty_paths.is_empty()
            && assessment.untracked_paths.is_empty())
    }
}
