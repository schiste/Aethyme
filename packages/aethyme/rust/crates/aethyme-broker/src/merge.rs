//! Merge simulation & promotion queue (Phase 5).
//!
//! Flow: `submit` snapshots a session's head into the queue, simulates
//! the merge onto the local integration branch (`git merge-tree`, no
//! worktree mutation), and — when clean — runs the affected gates on the
//! materialized merged tree in a throwaway worktree. Conflicts reject
//! the submission *before any gate runs* and write actionable
//! instructions into the session's worktree (the broker messages the
//! agent — decision 2026-07-10). `promote` advances the integration
//! branch to the verified merge commit; other queued entries whose base
//! moved are re-simulated.
//!
//! Boundary contract: promotion only advances the **local** integration
//! branch; publication is an explicit, confirmed `broker ship` operation.
//! The broker never opens PRs. The promotion trigger is a config setting
//! (`[promote] mode = "auto" | "manual"`) — **auto by
//! default** (decision 2026-07-13, after the first dogfood run: verified
//! means verified; holding it for a human command makes the human the
//! bottleneck). `mode = "manual"` restores explicit `broker promote`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::broker::{Broker, BrokerOpError};
use crate::gates::GateRunOutcome;
use crate::git::GitRepo;
use crate::types::{MergeQueueEntry, MergeStatus, SessionStatus};

pub const DEFAULT_INTEGRATION_BRANCH: &str = "aethyme/integration";
pub const ACTION_REQUIRED_RELPATH: &str = ".aethyme/broker-action-required.md";

/// `[promote]` section of `.aethyme/config.toml`.
#[derive(Debug, Clone)]
pub struct PromoteConfig {
    pub branch: String,
    pub auto: bool,
}

impl PromoteConfig {
    pub fn load(main_root: &Path) -> Self {
        let mut config = Self {
            branch: DEFAULT_INTEGRATION_BRANCH.to_string(),
            auto: true,
        };
        let Ok(text) = std::fs::read_to_string(main_root.join(".aethyme/config.toml")) else {
            return config;
        };
        let Ok(value) = text.parse::<toml::Value>() else {
            return config;
        };
        if let Some(promote) = value.get("promote") {
            if let Some(branch) = promote.get("branch").and_then(|v| v.as_str()) {
                config.branch = branch.to_string();
            }
            if let Some(mode) = promote.get("mode").and_then(|v| v.as_str()) {
                config.auto = mode != "manual";
            }
        }
        config
    }
}

/// Result of one submit: the queue entry after simulate (+ gates when
/// clean, + promotion when auto mode and verified).
#[derive(Debug, serde::Serialize)]
pub struct SubmitOutcome {
    pub entry: MergeQueueEntry,
    pub submission_plan: SubmissionPlan,
    pub conflicts: Vec<String>,
    pub conflict_details: Vec<SubmissionConflict>,
    pub gate_outcomes: Vec<GateRunOutcome>,
    /// True when every pending session-owned commit is already represented
    /// by integration or produces no tree change. No gate or promotion runs
    /// for this outcome.
    pub no_changes: bool,
    pub promoted: bool,
}

/// Whether a commit belongs to the session's recorded work boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionCommitOwnership {
    SessionOwned,
    InheritedFromRecordedBaseline,
    Ambiguous,
}

impl SubmissionCommitOwnership {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionOwned => "session_owned",
            Self::InheritedFromRecordedBaseline => "inherited_from_recorded_baseline",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// How a commit relates to the current integration history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionIntegrationState {
    Pending,
    AlreadyIntegratedByAncestry,
    AlreadyIntegratedByStablePatchIdentity,
    Ambiguous,
}

/// Full-SHA provenance for one commit carried by a submission.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SubmissionCommitProvenance {
    pub commit: String,
    pub parents: Vec<String>,
    pub ownership: SubmissionCommitOwnership,
    pub integration_state: SubmissionIntegrationState,
    pub patch_id: Option<String>,
    pub matching_integration_commits: Vec<String>,
}

/// Read-only explanation of the history a submit would currently carry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SubmissionPlan {
    pub session_id: i64,
    pub recorded_baseline: Option<String>,
    pub session_head: String,
    pub integration_head: String,
    pub safe: bool,
    pub commits: Vec<SubmissionCommitProvenance>,
    pub warnings: Vec<String>,
}

/// Provenance and recovery guidance for one surviving replay conflict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SubmissionConflict {
    pub path: String,
    pub originating_commit: String,
    pub ownership: SubmissionCommitOwnership,
    pub integration_side_commits: Vec<String>,
    pub remediation: String,
    pub commands: Vec<String>,
}

struct SubmissionReplay {
    tree: String,
    conflicts: Vec<String>,
    conflict_details: Vec<SubmissionConflict>,
}

impl Broker {
    /// Ensure the integration branch exists (created from the main
    /// checkout's HEAD with an explanatory event — issue #20) and return
    /// its current commit.
    pub fn integration_head(&mut self) -> Result<(String, String), BrokerOpError> {
        let config = PromoteConfig::load(&self.main_root_path());
        let repo = self.repo_handle();
        if let Some(commit) = repo.resolve_ref(&config.branch) {
            // Issue #40: the integration branch follows main. Fast-forward
            // it when it is an ancestor of the main checkout's HEAD — i.e.
            // everything it holds is already in main, so nothing promoted
            // can be lost. When it holds unmerged promotions (not
            // reachable from HEAD), it is left strictly alone.
            let head = repo.head_commit()?;
            if commit != head && repo.is_ancestor(&commit, &head) {
                repo.update_branch_ref(&config.branch, &head)?;
                let payload =
                    crate::events::integration_refreshed_payload(&config.branch, &commit, &head);
                self.store().append_event(
                    crate::events::MERGE_INTEGRATION_REFRESHED,
                    None,
                    Some(&payload),
                )?;
                return Ok((config.branch, head));
            }
            return Ok((config.branch, commit));
        }
        let head = repo.head_commit()?;
        repo.update_branch_ref(&config.branch, &head)?;
        self.store().append_event(
            crate::events::MERGE_INTEGRATION_BRANCH_CREATED,
            None,
            Some(&crate::events::integration_branch_created_payload(
                &config.branch,
                &head,
            )),
        )?;
        Ok((config.branch, head))
    }

    /// Submit a session's committed head for promotion: queue (idempotent
    /// per head), simulate, gate on the merged tree, and auto-promote if
    /// configured. Uncommitted changes are NOT included — the head commit
    /// is the unit of promotion.
    pub fn submit(&mut self, session_id: i64) -> Result<SubmitOutcome, BrokerOpError> {
        self.submit_with_policy(session_id, crate::gates::CachePolicy::Use)
    }

    /// Submit with an explicit policy for merged-tree gate cache lookup.
    pub fn submit_with_policy(
        &mut self,
        session_id: i64,
        cache_policy: crate::gates::CachePolicy,
    ) -> Result<SubmitOutcome, BrokerOpError> {
        let session = self.store().session(session_id)?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let head = checkout.head_commit()?;
        let ownership = self.audit_submit_ownership(session_id)?;
        if !ownership.ok {
            return Err(BrokerOpError::OwnershipViolation {
                summary: ownership.failure_summary(),
                report: Box::new(ownership),
            });
        }
        let (_branch, base) = self.integration_head()?;

        let entry = self.store().submit(session_id, &head, &base)?;
        // A new head supersedes this session's older in-flight entries:
        // without this, a stale conflicted entry would be re-simulated
        // (and re-announced) on every future promotion.
        let stale: Vec<i64> = self
            .store()
            .merge_queue()?
            .into_iter()
            .filter(|e| {
                e.session_id == session_id
                    && e.id != entry.id
                    && matches!(
                        e.status,
                        MergeStatus::Submitted
                            | MergeStatus::Simulating
                            | MergeStatus::Verified
                            | MergeStatus::Conflict
                            | MergeStatus::Rejected
                    )
            })
            .map(|e| e.id)
            .collect();
        for stale_id in stale {
            self.store()
                .set_merge_status(stale_id, MergeStatus::Superseded, None, None)?;
        }
        self.simulate_and_gate_with_policy(entry.id, cache_policy)
    }

    /// Simulate (and, when clean, gate) one queue entry against the
    /// CURRENT integration head. Rebinds the entry's base if the branch
    /// moved since submission.
    pub fn simulate_and_gate(&mut self, entry_id: i64) -> Result<SubmitOutcome, BrokerOpError> {
        self.simulate_and_gate_with_policy(entry_id, crate::gates::CachePolicy::Use)
    }

    fn simulate_and_gate_with_policy(
        &mut self,
        entry_id: i64,
        cache_policy: crate::gates::CachePolicy,
    ) -> Result<SubmitOutcome, BrokerOpError> {
        // #42: remember where the integration branch stood BEFORE the
        // follows-main refresh. Gate selection must diff against this —
        // for a main-checkout session the refresh fast-forwards the base
        // onto the session's own commits, and diffing against the
        // refreshed base yields an empty set (the observed `gates:[]`
        // vacuous verify). Diffing against the pre-refresh head selects
        // gates for the work actually being accepted; for worktree
        // sessions the two are equal (or the pre-refresh diff is a safe
        // superset when integration lagged main).
        let pre_refresh = {
            let branch = PromoteConfig::load(&self.main_root_path()).branch;
            self.repo_handle()
                .resolve_ref(&format!("refs/heads/{branch}"))
        };
        let (_branch, base) = self.integration_head()?;
        let entry = self
            .store()
            .merge_queue()?
            .into_iter()
            .find(|e| e.id == entry_id)
            .ok_or(crate::BrokerError::SessionNotFound(entry_id))?;
        let session = self.store().session(entry.session_id)?;
        let submission_plan = self.build_submission_plan(&session, &entry.head_commit, &base)?;

        let simulation = self.replay_submission_plan(&submission_plan)?;
        let base_tree = self.repo_handle().commit_tree_id(&base)?;
        let main_checkout_session = Path::new(&session.worktree_path) == self.main_root_path();
        self.store()
            .set_merge_status(entry.id, MergeStatus::Simulating, None, None)?;

        if !simulation.conflicts.is_empty() {
            // Conflict: reject before any gate runs, name the blocking
            // session when a lease matches, and message the agent via a
            // file drop in its worktree (#21).
            let blocking = self.blocking_sessions(entry.session_id, &simulation.conflicts)?;
            let details = serde_json::json!({
                "conflicts": simulation.conflicts,
                "conflict_details": simulation.conflict_details,
                "blocking_sessions": blocking,
                "base": base,
            });
            self.store().set_merge_status(
                entry.id,
                MergeStatus::Conflict,
                Some(&simulation.tree),
                Some(&details.to_string()),
            )?;
            write_action_required(
                Path::new(&session.worktree_path),
                &entry,
                &simulation.conflict_details,
                &blocking,
                &base,
            );
            let entry = self.queue_entry(entry.id)?;
            return Ok(SubmitOutcome {
                entry,
                submission_plan,
                conflicts: simulation.conflicts,
                conflict_details: simulation.conflict_details,
                gate_outcomes: Vec::new(),
                no_changes: false,
                promoted: false,
            });
        }

        // A clean replay that leaves integration's tree unchanged has
        // nothing to verify or promote. This is a normal terminal outcome
        // when every owned patch already landed (or is content-empty), and
        // a vital safety signal when a corrupted ownership baseline hid real
        // work: never manufacture a successful parent-tree promotion.
        // Primary-checkout sessions are the deliberate exception: the
        // follows-main refresh has already advanced integration to their
        // HEAD, and submit still needs to gate and record that externally
        // visible main movement.
        if simulation.tree == base_tree && !main_checkout_session {
            let details = serde_json::json!({
                "base": base,
                "reason": "submission produces no content change",
                "pending_session_owned_commits": submission_plan.commits.iter().filter(|commit| {
                    commit.ownership == SubmissionCommitOwnership::SessionOwned
                        && commit.integration_state == SubmissionIntegrationState::Pending
                }).map(|commit| commit.commit.clone()).collect::<Vec<_>>(),
            });
            self.store().record_content_empty_supersession(
                entry.id,
                &base,
                &simulation.tree,
                &details.to_string(),
            )?;
            clear_action_required(Path::new(&session.worktree_path));
            return Ok(SubmitOutcome {
                entry: self.queue_entry(entry.id)?,
                submission_plan,
                conflicts: Vec::new(),
                conflict_details: Vec::new(),
                gate_outcomes: Vec::new(),
                no_changes: true,
                promoted: false,
            });
        }

        // Clean merge: materialize as a commit and run affected gates on
        // it in a throwaway detached worktree (#22).
        let merge_commit = self.repo_handle().commit_tree(
            &simulation.tree,
            &[&base],
            &format!(
                "broker: promote session {} ({})",
                session.id,
                session.task.as_deref().unwrap_or("no task")
            ),
        )?;
        let verify_base = pre_refresh.as_deref().unwrap_or(&base);
        let changed = self
            .repo_handle()
            .changed_between(verify_base, &merge_commit)?;
        let tmp_dir = self
            .main_root_path()
            .join(".aethyme/run/merge-sim")
            .join(format!("entry-{}", entry.id));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let sim_worktree = self
            .repo_handle()
            .worktree_add_detached(&tmp_dir, &merge_commit)?;
        // Conflict-only brokering is valid: a repo with no gates.toml gets
        // textual merge simulation and promotion on clean merges, with zero
        // verification — recorded explicitly so nobody mistakes it for a
        // passing check run. A *malformed* gates.toml in the merged tree
        // stays a hard error (broken intent, not absent intent).
        let gates = match self.load_and_sync_gates_from(sim_worktree.root()) {
            Ok(gates) => gates,
            Err(BrokerOpError::GateConfig(crate::gates::GateConfigError::Missing(_))) => Vec::new(),
            Err(err) => {
                let _ = self.repo_handle().worktree_remove(&tmp_dir, true);
                return Err(err);
            }
        };
        let main_root = self.main_root_path();
        let gate_outcomes = crate::gates::run_affected(
            self.store(),
            &main_root,
            &sim_worktree,
            &gates,
            &changed,
            Some(entry.session_id),
            cache_policy,
        );
        let _ = self.repo_handle().worktree_remove(&tmp_dir, true);
        let gate_outcomes = gate_outcomes?;

        let all_pass = gate_outcomes
            .iter()
            .all(|o| o.status == crate::types::GateStatus::Pass);
        let details = serde_json::json!({
            "merge_commit": merge_commit,
            "base": base,
            "gates": gate_outcomes.iter().map(|o| serde_json::json!({
                "gate": o.gate,
                "tree_hash": o.tree_hash,
                "status": o.status,
                "failure_class": o.failure_class,
                "cached": o.cached,
            })).collect::<Vec<_>>(),
        });
        if all_pass {
            self.store().set_merge_status(
                entry.id,
                MergeStatus::Verified,
                Some(&simulation.tree),
                Some(&details.to_string()),
            )?;
        } else {
            self.store().set_merge_status(
                entry.id,
                MergeStatus::Rejected,
                Some(&simulation.tree),
                Some(&details.to_string()),
            )?;
        }

        let mut promoted = false;
        if all_pass && PromoteConfig::load(&self.main_root_path()).auto {
            self.promote(entry.id)?;
            promoted = true;
        }
        if promoted {
            clear_action_required(Path::new(&session.worktree_path));
        }
        let entry = self.queue_entry(entry.id)?;
        Ok(SubmitOutcome {
            entry,
            submission_plan,
            conflicts: Vec::new(),
            conflict_details: Vec::new(),
            gate_outcomes,
            no_changes: false,
            promoted,
        })
    }

    fn replay_submission_plan(
        &self,
        plan: &SubmissionPlan,
    ) -> Result<SubmissionReplay, BrokerOpError> {
        if !plan.safe {
            return Err(BrokerOpError::UnsafeSubmissionPlan {
                session_id: plan.session_id,
                reason: if plan.warnings.is_empty() {
                    "commit provenance is ambiguous".into()
                } else {
                    plan.warnings.join("; ")
                },
            });
        }

        let repo = self.repo_handle();
        let mut current = plan.integration_head.clone();
        for commit in &plan.commits {
            if commit.ownership != SubmissionCommitOwnership::SessionOwned
                || commit.integration_state != SubmissionIntegrationState::Pending
            {
                continue;
            }
            if commit.parents.len() != 1 {
                return Err(BrokerOpError::UnsupportedSubmissionCommit {
                    session_id: plan.session_id,
                    commit: commit.commit.clone(),
                    parent_count: commit.parents.len(),
                });
            }
            let simulation =
                repo.merge_tree_with_base(&commit.parents[0], &current, &commit.commit)?;
            if !simulation.conflicts.is_empty() {
                let conflict_details = simulation
                    .conflicts
                    .iter()
                    .map(|path| SubmissionConflict {
                        path: path.clone(),
                        originating_commit: commit.commit.clone(),
                        ownership: commit.ownership,
                        integration_side_commits: self
                            .integration_commits_touching_path(
                                &commit.parents[0],
                                &plan.integration_head,
                                path,
                            ),
                        remediation: format!(
                            "rebase session {} onto integration {}, resolve {} while replaying {}, then resubmit",
                            plan.session_id, plan.integration_head, path, commit.commit
                        ),
                        commands: vec![
                            format!("git fetch . {}", plan.integration_head),
                            format!("git rebase {}", plan.integration_head),
                            format!("aethyme broker submit --session {}", plan.session_id),
                        ],
                    })
                    .collect();
                return Ok(SubmissionReplay {
                    tree: simulation.tree,
                    conflicts: simulation.conflicts,
                    conflict_details,
                });
            }
            current = repo.commit_tree(
                &simulation.tree,
                &[&current],
                &format!(
                    "broker: replay {} for session {}",
                    &commit.commit[..12],
                    plan.session_id
                ),
            )?;
        }
        Ok(SubmissionReplay {
            tree: repo.commit_tree_id(&current)?,
            conflicts: Vec::new(),
            conflict_details: Vec::new(),
        })
    }

    fn integration_commits_touching_path(
        &self,
        session_parent: &str,
        integration_head: &str,
        path: &str,
    ) -> Vec<String> {
        let repo = self.repo_handle();
        let Ok(base) = repo.merge_base(session_parent, integration_head) else {
            return Vec::new();
        };
        let Ok(commits) = repo.first_parent_commits_between_oldest(&base, integration_head) else {
            return Vec::new();
        };
        commits
            .into_iter()
            .filter(|commit| {
                repo.first_parent(commit)
                    .and_then(|parent| repo.changed_between(&parent, commit))
                    .is_ok_and(|paths| paths.iter().any(|candidate| candidate == path))
            })
            .collect()
    }

    pub(crate) fn build_submission_plan(
        &self,
        session: &crate::types::Session,
        session_head: &str,
        integration_head: &str,
    ) -> Result<SubmissionPlan, BrokerOpError> {
        let repo = self.repo_handle();
        // Once a contribution has been promoted, ownership begins after the
        // last accepted session HEAD. `diff_base` remains the operational
        // fallback for a session that has never contributed.
        let accepted_checkpoint = session.accepted_session_head.as_deref();
        let Some(recorded_baseline) = accepted_checkpoint.or(session.diff_base.as_deref()) else {
            return Ok(SubmissionPlan {
                session_id: session.id,
                recorded_baseline: None,
                session_head: session_head.to_string(),
                integration_head: integration_head.to_string(),
                safe: false,
                commits: Vec::new(),
                warnings: vec![
                    "session has no recorded baseline; commit ownership is ambiguous".into(),
                ],
            });
        };

        let mut warnings = Vec::new();
        let (owned, inherited) = if repo.is_ancestor(recorded_baseline, session_head) {
            let owned = repo.commits_between_oldest(recorded_baseline, session_head)?;
            let inherited = if accepted_checkpoint.is_some() {
                // The checkpoint and everything before it already passed
                // promotion. A replayed integration commit commonly has a
                // different SHA, so ancestry alone must not reintroduce that
                // proven contribution as inherited work.
                Vec::new()
            } else {
                let owned_set = owned.iter().cloned().collect::<BTreeSet<_>>();
                repo.commits_excluding_oldest(session_head, integration_head)?
                    .into_iter()
                    .filter(|commit| !owned_set.contains(commit))
                    .collect::<Vec<_>>()
            };
            (owned, inherited)
        } else if repo.is_ancestor(integration_head, session_head) {
            warnings.push(format!(
                "recorded baseline {recorded_baseline} was rewritten; ownership uses the unambiguous rebased range {integration_head}..{session_head}"
            ));
            (
                repo.commits_between_oldest(integration_head, session_head)?,
                Vec::new(),
            )
        } else {
            return Ok(SubmissionPlan {
                session_id: session.id,
                recorded_baseline: Some(recorded_baseline.to_string()),
                session_head: session_head.to_string(),
                integration_head: integration_head.to_string(),
                safe: false,
                commits: repo
                    .commits_excluding_oldest(session_head, integration_head)?
                    .into_iter()
                    .map(|commit| {
                        let parents = repo.commit_parents(&commit)?;
                        Ok(SubmissionCommitProvenance {
                            commit,
                            parents,
                            ownership: SubmissionCommitOwnership::Ambiguous,
                            integration_state: SubmissionIntegrationState::Ambiguous,
                            patch_id: None,
                            matching_integration_commits: Vec::new(),
                        })
                    })
                    .collect::<Result<Vec<_>, BrokerOpError>>()?,
                warnings: vec![format!(
                    "recorded baseline {recorded_baseline} is not an ancestor of session HEAD {session_head}; commit ownership is ambiguous"
                )],
            });
        };

        let integration_candidates =
            repo.first_parent_commits_excluding_oldest(integration_head, recorded_baseline)?;
        let mut integration_by_patch: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for commit in integration_candidates {
            let parents = repo.commit_parents(&commit)?;
            let Some(parent) = parents.first() else {
                continue;
            };
            if let Some(patch_id) = repo.patch_id_between(parent, &commit)? {
                integration_by_patch
                    .entry(patch_id)
                    .or_default()
                    .push(commit);
            }
        }

        let mut commits = Vec::with_capacity(inherited.len() + owned.len());
        for (commit, ownership) in inherited
            .into_iter()
            .map(|commit| {
                (
                    commit,
                    SubmissionCommitOwnership::InheritedFromRecordedBaseline,
                )
            })
            .chain(
                owned
                    .into_iter()
                    .map(|commit| (commit, SubmissionCommitOwnership::SessionOwned)),
            )
        {
            let parents = repo.commit_parents(&commit)?;
            let patch_id = parents
                .first()
                .map(|parent| repo.patch_id_between(parent, &commit))
                .transpose()?
                .flatten();
            let matching_integration_commits = patch_id
                .as_ref()
                .and_then(|patch| integration_by_patch.get(patch))
                .cloned()
                .unwrap_or_default();
            let integration_state = if repo.is_ancestor(&commit, integration_head) {
                SubmissionIntegrationState::AlreadyIntegratedByAncestry
            } else {
                match matching_integration_commits.len() {
                    0 => SubmissionIntegrationState::Pending,
                    1 => SubmissionIntegrationState::AlreadyIntegratedByStablePatchIdentity,
                    _ => SubmissionIntegrationState::Ambiguous,
                }
            };
            commits.push(SubmissionCommitProvenance {
                commit,
                parents,
                ownership,
                integration_state,
                patch_id,
                matching_integration_commits,
            });
        }

        let ambiguous = commits.iter().any(|commit| {
            commit.ownership == SubmissionCommitOwnership::Ambiguous
                || commit.integration_state == SubmissionIntegrationState::Ambiguous
        });
        if ambiguous {
            warnings.push(
                "one or more commits have ambiguous provenance; normalized replay must refuse this plan"
                    .into(),
            );
        }
        Ok(SubmissionPlan {
            session_id: session.id,
            recorded_baseline: Some(recorded_baseline.to_string()),
            session_head: session_head.to_string(),
            integration_head: integration_head.to_string(),
            safe: !ambiguous,
            commits,
            warnings,
        })
    }

    /// Advance the integration branch to a verified entry's merge commit,
    /// then re-simulate every other non-terminal entry whose base moved.
    /// Publication is a separate, explicit `broker ship` operation.
    pub fn promote(&mut self, entry_id: i64) -> Result<(), BrokerOpError> {
        let entry = self.queue_entry(entry_id)?;
        if entry.status != MergeStatus::Verified {
            return Err(BrokerOpError::NotVerified {
                entry: entry_id,
                status: entry.status.as_str(),
            });
        }
        let details: serde_json::Value = entry
            .details_json
            .as_deref()
            .and_then(|d| serde_json::from_str(d).ok())
            .unwrap_or_default();
        let base_at_verify = details
            .get("base")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let merge_commit = details
            .get("merge_commit")
            .and_then(|v| v.as_str())
            .ok_or(crate::BrokerError::SessionNotFound(entry_id))?
            .to_string();

        let (branch, current_base) = self.integration_head()?;
        if current_base != base_at_verify {
            // Base moved since verification: verification is stale —
            // re-simulate instead of promoting a stale merge.
            let outcome = self.simulate_and_gate(entry_id)?;
            if outcome.entry.status != MergeStatus::Verified {
                return Err(BrokerOpError::NotVerified {
                    entry: entry_id,
                    status: outcome.entry.status.as_str(),
                });
            }
            return self.promote(entry_id);
        }

        self.repo_handle()
            .update_branch_ref(&branch, &merge_commit)?;
        self.store().record_merge_promotion(
            entry_id,
            &merge_commit,
            &crate::events::merge_promoted_payload(&branch, &merge_commit),
        )?;

        // Requeue: everything still in flight was verified/conflicted
        // against a base that just moved (#23).
        let stale: Vec<i64> = self
            .store()
            .merge_queue()?
            .into_iter()
            .filter(|e| {
                e.id != entry_id
                    && matches!(
                        e.status,
                        MergeStatus::Submitted
                            | MergeStatus::Simulating
                            | MergeStatus::Verified
                            | MergeStatus::Conflict
                    )
            })
            .map(|e| e.id)
            .collect();
        for stale_id in stale {
            // Promoting an entry can recursively re-simulate and promote
            // another queued entry. Re-read both queue and session state so
            // the outer stale snapshot cannot promote that entry twice, and
            // never resurrect abandoned work owned by a cleaned session.
            let Ok(stale_entry) = self.queue_entry(stale_id) else {
                continue;
            };
            if !matches!(
                stale_entry.status,
                MergeStatus::Submitted
                    | MergeStatus::Simulating
                    | MergeStatus::Verified
                    | MergeStatus::Conflict
            ) {
                continue;
            }
            let Ok(stale_session) = self.store().session(stale_entry.session_id) else {
                continue;
            };
            if stale_session.status == SessionStatus::Cleaned {
                let _ = self.store().set_merge_status(
                    stale_id,
                    MergeStatus::Superseded,
                    None,
                    Some("{\"reason\":\"session cleaned before queue revalidation\"}"),
                );
                continue;
            }
            // Best effort: a failure re-simulating one entry must not
            // abort the promotion that already happened.
            let _ = self.simulate_and_gate(stale_id);
        }
        Ok(())
    }

    fn queue_entry(&mut self, entry_id: i64) -> Result<MergeQueueEntry, BrokerOpError> {
        self.store()
            .merge_queue()?
            .into_iter()
            .find(|e| e.id == entry_id)
            .ok_or(crate::BrokerError::SessionNotFound(entry_id).into())
    }

    /// Sessions (other than `submitter`) holding active leases on any of
    /// the conflicted paths — the "who am I fighting with" signal.
    fn blocking_sessions(
        &mut self,
        submitter: i64,
        conflicts: &[String],
    ) -> Result<Vec<i64>, BrokerOpError> {
        let leases = self.store().active_leases()?;
        let mut blocking: Vec<i64> = leases
            .iter()
            .filter(|lease| lease.session_id != submitter)
            .filter(|lease| {
                conflicts.iter().any(|conflict| {
                    conflict == &lease.path
                        || (lease.path.ends_with('/') && conflict.starts_with(&lease.path))
                })
            })
            .map(|lease| lease.session_id)
            .collect();
        blocking.sort_unstable();
        blocking.dedup();
        Ok(blocking)
    }
}

/// The agent-facing conflict message (#21): machine- and human-readable,
/// dropped into the session's worktree. The Aethyme-generated AGENTS.md
/// can point agents at this path; no vendor-specific injection.
impl Broker {
    /// The commit a session's own changes should be measured against:
    /// `merge-base(session HEAD, integration)`. Unlike the stored
    /// adoption-time `diff_base`, this self-heals after a conflict-rebase
    /// (#41): promoted commits brought in by the rebase move into the
    /// common ancestry instead of inflating the session's apparent diff
    /// (the phantom-lease symptom). Read-only — never refreshes the
    /// integration branch.
    pub fn session_change_base(&mut self, session_checkout: &GitRepo) -> Option<String> {
        let integration = self.integration_tip()?;
        session_checkout.merge_base(&integration, "HEAD").ok()
    }

    /// The integration branch's current commit, without creating or
    /// refreshing the branch (read-only, unlike [`Self::integration_head`]).
    /// Invariant across sessions — resolve once and reuse when deriving
    /// change bases in a loop (`refresh_leases` did this per session,
    /// the dominant redundant git call in `broker status`).
    pub(crate) fn integration_tip(&self) -> Option<String> {
        let branch = PromoteConfig::load(&self.main_root_path()).branch;
        self.repo_handle()
            .resolve_ref(&format!("refs/heads/{branch}"))
    }

    /// Resolve the integration view without creating or fast-forwarding its
    /// ref. Before the first promotion, a snapshot reports main as the
    /// effective integration base while leaving the ref absent.
    pub(crate) fn integration_head_snapshot(&self) -> Result<(String, String), BrokerOpError> {
        let branch = PromoteConfig::load(&self.main_root_path()).branch;
        let commit = self
            .repo_handle()
            .resolve_ref(&format!("refs/heads/{branch}"))
            .map(Ok)
            .unwrap_or_else(|| self.repo_handle().head_commit())?;
        Ok((branch, commit))
    }
}

/// Remove a stale action-required drop once the session's work promotes
/// (#41 follow-on, reported by agent A4: the file survived success with
/// outdated blocking info). Best-effort — the worktree may be gone.
fn clear_action_required(worktree: &Path) {
    let _ = std::fs::remove_file(worktree.join(ACTION_REQUIRED_RELPATH));
}

fn write_action_required(
    worktree: &Path,
    entry: &MergeQueueEntry,
    conflicts: &[SubmissionConflict],
    blocking: &[i64],
    base: &str,
) {
    let blocking_note = if blocking.is_empty() {
        "No live session currently holds leases on these paths.".to_string()
    } else {
        format!(
            "Blocking session(s): {} — coordinate or wait for their promotion.",
            blocking
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let body = format!(
        "# Broker: action required — merge conflict\n\n\
         Your submission (commit `{head}`) conflicts with the integration\n\
         branch (base `{base}`) and was rejected before any CI ran.\n\n\
         Conflicting files:\n{files}\n\n{blocking_note}\n\n\
         To resolve, in this worktree:\n\n\
         1. `git fetch . {base}` (the base is a local commit; no network)\n\
         2. `git rebase {base}` and resolve the conflicts above\n\
            (headless agents: if the rebase pauses, continue with\n\
            `GIT_EDITOR=true git rebase --continue` — never rely on an\n\
            interactive editor)\n\
         3. resubmit: `aethyme broker submit --session {session}`\n\n\
         This file is regenerated on each conflicted submission; it is\n\
         gitignored broker state (delete freely).\n",
        head = entry.head_commit,
        base = base,
        files = conflicts
            .iter()
            .map(|conflict| {
                let integration = if conflict.integration_side_commits.is_empty() {
                    "unknown".to_string()
                } else {
                    conflict.integration_side_commits.join(", ")
                };
                format!(
                    "- `{path}` — session commit `{origin}` ({ownership}); integration commit(s): {integration}",
                    path = conflict.path,
                    origin = conflict.originating_commit,
                    ownership = conflict.ownership.as_str(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        blocking_note = blocking_note,
        session = entry.session_id,
    );
    let path = worktree.join(ACTION_REQUIRED_RELPATH);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, body);
}
