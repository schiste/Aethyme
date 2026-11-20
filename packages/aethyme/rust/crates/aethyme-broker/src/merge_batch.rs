//! Verify an explicitly ready group of independent published branches against
//! one fetched base. A candidate is an object, with no queue or ref update.

use super::*;

const MAX_BATCH_MEMBERS: usize = 16;

#[path = "merge_batch_queue.rs"]
mod queue;
pub use queue::Receipt as QueuedBatchReceipt;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BatchMember {
    pub session_id: i64,
    pub head: String,
}

pub(crate) struct BatchRequest {
    base: String,
    members: Vec<BatchMember>,
}

impl BatchRequest {
    pub(crate) fn parse(base: &str, members: &[String]) -> Option<Self> {
        fn object_id(value: &str) -> bool {
            matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        }

        if !object_id(base) || !(2..=MAX_BATCH_MEMBERS).contains(&members.len()) {
            return None;
        }
        let mut ids = BTreeSet::new();
        let mut heads = BTreeSet::new();
        let members = members
            .iter()
            .map(|value| {
                let (id, head) = value.split_once('=')?;
                let id: i64 = id.parse().ok()?;
                let head = head.to_ascii_lowercase();
                (id > 0 && object_id(&head) && ids.insert(id) && heads.insert(head.clone()))
                    .then_some(BatchMember {
                        session_id: id,
                        head,
                    })
            })
            .collect::<Option<_>>()?;
        Some(Self {
            base: base.to_ascii_lowercase(),
            members,
        })
    }
}

#[derive(serde::Serialize)]
pub(crate) struct BatchVerification {
    pub(crate) state: &'static str,
    base: String,
    members: Vec<BatchMember>,
    candidate: Option<String>,
    tree: Option<String>,
    conflicting_session: Option<i64>,
    conflict_paths: Vec<String>,
    graph_advice: Option<String>,
    gates: Vec<GateRunOutcome>,
}

impl BatchVerification {
    fn new(request: BatchRequest) -> Self {
        Self {
            state: "superseded",
            base: request.base,
            members: request.members,
            candidate: None,
            tree: None,
            conflicting_session: None,
            conflict_paths: Vec::new(),
            graph_advice: None,
            gates: Vec::new(),
        }
    }

    pub(crate) fn metric_details(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state,
            "members": self.members.len(),
            "executed_gates": self.gates.iter().filter(|gate| !gate.cached).count(),
            "cached_gates": self.gates.iter().filter(|gate| gate.cached).count(),
            "executed_gate_ms": self.gates.iter().filter(|gate| !gate.cached).filter_map(|gate| gate.duration_ms).sum::<i64>(),
        })
    }
}

impl Broker {
    pub(crate) fn verify_batch(
        &mut self,
        coordinator_id: i64,
        request: BatchRequest,
        cache: crate::gates::CachePolicy,
    ) -> Result<BatchVerification, BrokerOpError> {
        self.verify_batch_in_slot(coordinator_id, request, cache, None)
    }

    fn verify_batch_in_slot(
        &mut self,
        coordinator_id: i64,
        request: BatchRequest,
        cache: crate::gates::CachePolicy,
        execution: Option<QueuedBatchExecution<'_>>,
    ) -> Result<BatchVerification, BrokerOpError> {
        let admission = crate::phase_telemetry::span("submit_batch_admission");
        let mut report = BatchVerification::new(request);
        let coordinator = self.store_ref().session(coordinator_id)?;
        if coordinator.status.is_closed() {
            return Err(batch_refusal(
                coordinator_id,
                "the coordinator session is closed",
            ));
        }
        if crate::git::repository_routing_environment_overridden() {
            return Err(batch_refusal(
                coordinator_id,
                "Git routing or configuration is overridden",
            ));
        }
        self.check_submission_root_before_planning()?;
        let main = self.main_root_path();
        let repo = GitRepo::discover(&main)?;
        let mut standalone_slot;
        let (slot, verifier, progress) = if let Some(execution) = execution {
            (execution.slot, execution.verifier, execution.progress)
        } else {
            let root_plan = self.worktree_root_plan()?;
            let Some(slot) =
                crate::verification::ExactTreeVerificationSlot::try_acquire_with_worktree_root_plan(
                    &main,
                    "merge-batch",
                    &root_plan,
                )?
            else {
                report.state = "busy";
                return Ok(report);
            };
            standalone_slot = slot;
            (&mut standalone_slot, coordinator_id, None)
        };
        let Some(history) =
            capture_submission_plan_history_view(&main, self.repo_handle(), self.store_ref())
        else {
            return Err(batch_refusal(
                coordinator_id,
                "the coordinated history view could not be proven",
            ));
        };
        if !self.batch_is_current(&repo, &report.base, &report.members)? {
            return Ok(report);
        }
        self.require_trusted_policy_at_commit(&report.base, Some(coordinator_id))?;
        drop(admission);

        let compose = crate::phase_telemetry::span("submit_batch_compose");
        let mut tip = report.base.clone();
        let mut parents = vec![report.base.as_str()];
        let attribution = crate::attribution::Attribution::broker_only();
        let base_seconds = repo.oldest_commit_time_ms(&[&report.base]).ok_or_else(|| {
            batch_refusal(coordinator_id, "cannot read the exact base commit date")
        })? / 1000;
        let date = format!("@{base_seconds} +0000");
        let mut inputs = BTreeSet::new();
        let mut pending_messages = Vec::new();
        for member in &report.members {
            let ownership = self.audit_submit_ownership(member.session_id)?;
            if !ownership.ok {
                return Err(BrokerOpError::OwnershipViolation {
                    summary: ownership.failure_summary(),
                    report: Box::new(ownership),
                });
            }
            let session = self.store_ref().session(member.session_id)?;
            let plan = self.build_submission_plan(&session, &member.head, &report.base)?;
            let actual: BTreeSet<_> = repo
                .commits_excluding_oldest(&member.head, &report.base)?
                .into_iter()
                .collect();
            let pending_order = plan.pending_owned_commit_ids();
            let pending: BTreeSet<_> = pending_order.iter().cloned().collect();
            if !plan.safe
                || actual.is_empty()
                || actual != pending
                || !repo.is_ancestor_checked(&report.base, &member.head)?
                || !inputs.is_disjoint(&pending)
            {
                return Err(batch_refusal(
                    member.session_id,
                    "batch requires independent, wholly owned pending commits based on the exact default branch. Sync or submit this session individually",
                ));
            }
            inputs.extend(pending);
            for commit in pending_order {
                pending_messages.push(repo.commit_message(&commit)?);
            }
            let first_member = parents.len() == 1;
            parents.push(&member.head);
            if first_member {
                tip.clone_from(&member.head);
                continue;
            }
            let merged = repo.merge_tree_with_base(&report.base, &tip, &member.head)?;
            if !merged.conflicts.is_empty() {
                report.state = "conflict";
                report.tree = None;
                report.conflicting_session = Some(member.session_id);
                report.conflict_paths = merged.conflicts;
                return Ok(report);
            }
            let mut message =
                String::from("chore(broker): combine published session heads for verification");
            if parents.len() == report.members.len() + 1 {
                append_verification_contract(&mut message, &pending_messages.join("\n"));
            }
            tip = repo.commit_tree_with_date(
                &merged.tree,
                &parents,
                &message,
                &attribution,
                Some(&date),
            )?;
            report.tree = Some(merged.tree);
        }
        let tree = report
            .tree
            .as_deref()
            .expect("batch has at least two members");
        if tree == repo.commit_tree_id(&report.base)? {
            report.state = "no_candidate_changes";
            return Ok(report);
        }
        let changed = repo.gate_scope_changed_between(&report.base, &tip)?;
        let gates = self.load_and_sync_gates_at_commit(&report.base)?;
        let graph_policy =
            crate::graph_integrity::load_graph_policy_at_commit(&repo, &report.base)?;
        drop(compose);

        let checkout = crate::phase_telemetry::span("submit_batch_checkout");
        let candidate = slot.materialize(&repo, &tip)?;
        if !self.batch_is_current(&repo, &report.base, &report.members)?
            || validate_submission_history_view(
                &main,
                self.repo_handle(),
                self.store_ref(),
                &history,
            )
            .is_err()
        {
            return Ok(report);
        }
        drop(checkout);

        let verify = crate::phase_telemetry::span("submit_batch_verify");
        let graph = crate::graph_integrity::verify_disposable_checkout_of_tree(
            &candidate,
            &graph_policy,
            tree,
        );
        report.graph_advice = graph.advice();
        if let Some(path) = self.session_scratch_dir(verifier) {
            crate::session_scratch::remember_for_commands(verifier, &path);
        }
        if let Some(progress) = progress {
            progress.subphase("executing gates");
        }
        let storage = self.storage_context().gate_storage_locations();
        report.gates = crate::gates::run_affected_for_submit_in(
            self.store(),
            &main,
            &candidate,
            gates.as_deref().unwrap_or_default(),
            &changed,
            &report.base,
            Some(verifier),
            cache,
            progress,
            storage,
        )?;
        slot.cleanup();
        drop(verify);

        let _finalize = crate::phase_telemetry::span("submit_batch_revalidate");
        if !self.batch_is_current(&repo, &report.base, &report.members)?
            || validate_submission_history_view(
                &main,
                self.repo_handle(),
                self.store_ref(),
                &history,
            )
            .is_err()
        {
            return Ok(report);
        }
        report.candidate = Some(tip);
        report.state = if gates.is_none() {
            "no_gate_configuration"
        } else if report.gates.is_empty() {
            "no_gates_triggered"
        } else {
            match gate_verdict(&report.gates) {
                GateVerdict::Passed => "verified",
                GateVerdict::Deferred => "deferred",
                GateVerdict::Failed => "failed",
            }
        };
        Ok(report)
    }

    fn batch_is_current(
        &self,
        repo: &GitRepo,
        base: &str,
        members: &[BatchMember],
    ) -> Result<bool, BrokerOpError> {
        let Some(default) = crate::session_push::tracked_default(repo) else {
            return Ok(false);
        };
        if default.commit != base {
            return Ok(false);
        }
        for member in members {
            let session = self.store_ref().session(member.session_id)?;
            if session.status.is_closed() {
                return Ok(false);
            }
            let (head, published) = self.session_push_tips(&session, Some(&default))?;
            if head != member.head || published.as_deref() != Some(&member.head) {
                return Ok(false);
            }
            let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
            if checkout.root() == repo.root()
                || checkout.root() != Path::new(&session.worktree_path)
                || checkout.main_root()? != repo.root()
                || require_session_checkout_identity(&session, &checkout, Some(&member.head))
                    .is_err()
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

struct QueuedBatchExecution<'a> {
    slot: &'a mut crate::verification::ExactTreeVerificationSlot,
    verifier: i64,
    progress: Option<&'a crate::submit_progress::SubmitProgress>,
}

pub(super) enum QueuedAdmission {
    Single(crate::verification::ExactTreeVerificationSlot),
    Verified(QueuedBatchReceipt),
}

impl Broker {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn admit_queued_verification(
        &mut self,
        main: &Path,
        root_plan: &crate::WorktreeRootPlan,
        entry: &MergeQueueEntry,
        base: &str,
        paths: &[String],
        coordinator: Option<i64>,
        cache: crate::gates::CachePolicy,
        progress: Option<&crate::submit_progress::SubmitProgress>,
    ) -> Result<QueuedAdmission, BrokerOpError> {
        let report_wait = |waited| {
            if let Some(progress) = progress {
                progress.report_slot_wait(main, waited);
            }
        };
        let acquire_single = || {
            let mut report = report_wait;
            crate::verification::ExactTreeVerificationSlot::acquire_pooled_reporting_with_worktree_root_plan(
                main,
                "merge-sim",
                MERGE_VERIFICATION_SLOTS,
                root_plan,
                crate::gates::heartbeat_interval(),
                &mut report,
            ).map(QueuedAdmission::Single)
        };
        let Some(coordinator) = coordinator else {
            return acquire_single();
        };
        let mut ticket = None;
        let mut attempted = false;
        let mut slot = crate::verification::ExactTreeVerificationSlot::acquire_pooled_until(
            main,
            "merge-sim",
            MERGE_VERIFICATION_SLOTS,
            root_plan,
            crate::gates::heartbeat_interval(),
            &mut |waited, report| {
                if report {
                    report_wait(waited);
                }
                if !std::mem::replace(&mut attempted, true) {
                    let _phase = crate::phase_telemetry::span("submit_queued_batch_register");
                    let member = BatchMember {
                        session_id: entry.session_id,
                        head: entry.head_commit.clone(),
                    };
                    // This probe runs only after every slot is observed busy.
                    if GitRepo::discover(main).is_ok_and(|repo| {
                        self.batch_is_current(&repo, base, std::slice::from_ref(&member))
                            .unwrap_or(false)
                    }) {
                        ticket = queue::Ticket::enqueue(
                            main,
                            coordinator,
                            member,
                            base,
                            paths,
                            matches!(cache, crate::gates::CachePolicy::Bypass),
                        );
                    }
                }
                match ticket.as_ref().map(queue::Ticket::has_claim).transpose() {
                    Ok(claimed) => claimed.unwrap_or(false),
                    Err(_) => {
                        drop(ticket.take());
                        false
                    }
                }
            },
        )?;
        let ready_cutoff = crate::file_lock::host_monotonic_ns();
        let Some(ticket) = ticket else {
            return Ok(QueuedAdmission::Single(
                slot.expect("no ticket can redirect"),
            ));
        };
        let admission = if slot.is_some() {
            let _phase = crate::phase_telemetry::span("submit_queued_batch_claim");
            ticket.admit(ready_cutoff)
        } else {
            Ok(queue::Admission::Follower)
        };
        match admission {
            Ok(queue::Admission::Leader(request)) if ticket.claim_is_live() => {
                let members = request.members.len();
                if let Some(progress) = progress {
                    progress.phase(crate::submit_progress::PHASE_VERIFYING);
                }
                let slot = slot.as_mut().expect("a batch verifier holds the slot");
                let verified = self.verify_batch_in_slot(
                    coordinator,
                    request,
                    cache,
                    Some(QueuedBatchExecution {
                        slot,
                        verifier: entry.session_id,
                        progress,
                    }),
                );
                if verified.is_err() {
                    slot.cleanup();
                }
                let receipt = match verified {
                    Ok(result) => {
                        crate::phase_telemetry::attach(
                            "batch_verification",
                            result.metric_details(),
                        );
                        match (result.state, result.candidate, result.tree) {
                            ("verified", Some(candidate), Some(tree)) => Some(QueuedBatchReceipt {
                                coordinator,
                                verifier: entry.session_id,
                                base: result.base,
                                members: result.members,
                                candidate,
                                tree,
                                gates: serde_json::to_value(result.gates)
                                    .expect("a gate outcome serializes to JSON"),
                            }),
                            _ => None,
                        }
                    }
                    Err(_) => None,
                };
                let _phase = crate::phase_telemetry::span("submit_queued_batch_handoff");
                let receipt = receipt
                    .filter(|receipt| self.queued_receipt_is_current(coordinator, entry, receipt));
                let receipt = ticket.complete(receipt).ok().flatten();
                record_queued_batch(
                    "leader",
                    if receipt.is_some() {
                        "verified"
                    } else {
                        "fallback"
                    },
                    Some(members),
                );
                if let Some(receipt) = receipt {
                    return Ok(QueuedAdmission::Verified(receipt));
                }
            }
            Ok(queue::Admission::Follower) => {
                drop(slot);
                if let Some(progress) = progress {
                    progress.phase("waiting for shared verification");
                }
                let phase = crate::phase_telemetry::span("submit_queued_batch_follow");
                if let Some(receipt) = ticket.follow()
                    && self.queued_receipt_is_current(coordinator, entry, &receipt)
                {
                    record_queued_batch("follower", "verified", Some(receipt.members.len()));
                    return Ok(QueuedAdmission::Verified(receipt));
                }
                record_queued_batch("follower", "fallback", None);
                drop(phase);
                drop(ticket);
                if let Some(progress) = progress {
                    progress.phase(crate::submit_progress::PHASE_WAITING_FOR_SLOT);
                }
                return acquire_single();
            }
            Ok(queue::Admission::Single) => record_queued_batch("single", "no_cohort", None),
            _ => record_queued_batch("single", "fallback", None),
        }
        Ok(QueuedAdmission::Single(
            slot.expect("a redirect follows the accepted verifier"),
        ))
    }

    fn queued_receipt_is_current(
        &self,
        coordinator: i64,
        entry: &MergeQueueEntry,
        receipt: &QueuedBatchReceipt,
    ) -> bool {
        let Ok(repo) = GitRepo::discover(&self.main_root_path()) else {
            return false;
        };
        receipt.coordinator == coordinator
            && receipt.base == entry.base_commit
            && receipt.members.iter().any(|member| {
                member.session_id == entry.session_id && member.head == entry.head_commit
            })
            && repo
                .commit_tree_id(&receipt.candidate)
                .is_ok_and(|tree| tree == receipt.tree)
            && self
                .batch_is_current(&repo, &receipt.base, &receipt.members)
                .unwrap_or(false)
    }
}

fn record_queued_batch(role: &'static str, outcome: &'static str, members: Option<usize>) {
    crate::phase_telemetry::attach(
        "queued_batch",
        serde_json::json!({
            "role": role,
            "outcome": outcome,
            "members": members,
        }),
    );
}

fn batch_refusal(id: i64, reason: &str) -> BrokerOpError {
    BrokerOpError::UnsafeSubmissionPlan {
        session_id: id,
        reason: reason.into(),
    }
}
