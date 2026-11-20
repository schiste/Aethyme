//! Verify an explicitly ready group of independent published branches against
//! one fetched base. A candidate is an object, with no queue or ref update.

use super::*;

const MAX_BATCH_MEMBERS: usize = 16;

#[derive(serde::Serialize)]
pub(crate) struct BatchMember {
    session_id: i64,
    head: String,
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
        let root_plan = self.worktree_root_plan()?;
        let Some(mut slot) =
            crate::verification::ExactTreeVerificationSlot::try_acquire_with_worktree_root_plan(
                &main,
                "merge-batch",
                &root_plan,
            )?
        else {
            report.state = "busy";
            return Ok(report);
        };
        let Some(history) =
            capture_submission_plan_history_view(&main, self.repo_handle(), self.store_ref())
        else {
            return Err(batch_refusal(
                coordinator_id,
                "the coordinated history view could not be proven",
            ));
        };
        if !self.batch_is_current(&repo, &report)? {
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
        if !self.batch_is_current(&repo, &report)?
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
        if let Some(path) = self.session_scratch_dir(coordinator_id) {
            crate::session_scratch::remember_for_commands(coordinator_id, &path);
        }
        let storage = self.storage_context().gate_storage_locations();
        report.gates = crate::gates::run_affected_for_submit_in(
            self.store(),
            &main,
            &candidate,
            gates.as_deref().unwrap_or_default(),
            &changed,
            &report.base,
            Some(coordinator_id),
            cache,
            None,
            storage,
        )?;
        slot.cleanup();
        drop(verify);

        let _finalize = crate::phase_telemetry::span("submit_batch_revalidate");
        if !self.batch_is_current(&repo, &report)?
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
        report: &BatchVerification,
    ) -> Result<bool, BrokerOpError> {
        let Some(default) = crate::session_push::tracked_default(repo) else {
            return Ok(false);
        };
        if default.commit != report.base {
            return Ok(false);
        }
        for member in &report.members {
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

fn batch_refusal(id: i64, reason: &str) -> BrokerOpError {
    BrokerOpError::UnsafeSubmissionPlan {
        session_id: id,
        reason: reason.into(),
    }
}
