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
    pub conflicts: Vec<String>,
    pub gate_outcomes: Vec<GateRunOutcome>,
    pub promoted: bool,
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
        self.simulate_and_gate(entry.id)
    }

    /// Simulate (and, when clean, gate) one queue entry against the
    /// CURRENT integration head. Rebinds the entry's base if the branch
    /// moved since submission.
    pub fn simulate_and_gate(&mut self, entry_id: i64) -> Result<SubmitOutcome, BrokerOpError> {
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

        self.store()
            .set_merge_status(entry.id, MergeStatus::Simulating, None, None)?;
        let simulation = self
            .repo_handle()
            .merge_tree_simulate(&base, &entry.head_commit)?;

        if !simulation.conflicts.is_empty() {
            // Conflict: reject before any gate runs, name the blocking
            // session when a lease matches, and message the agent via a
            // file drop in its worktree (#21).
            let blocking = self.blocking_sessions(entry.session_id, &simulation.conflicts)?;
            let details = serde_json::json!({
                "conflicts": simulation.conflicts,
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
                &simulation.conflicts,
                &blocking,
                &base,
            );
            let entry = self.queue_entry(entry.id)?;
            return Ok(SubmitOutcome {
                entry,
                conflicts: simulation.conflicts,
                gate_outcomes: Vec::new(),
                promoted: false,
            });
        }

        // Clean merge: materialize as a commit and run affected gates on
        // it in a throwaway detached worktree (#22).
        let merge_commit = self.repo_handle().commit_tree(
            &simulation.tree,
            &[&base, &entry.head_commit],
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
            conflicts: Vec::new(),
            gate_outcomes,
            promoted,
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
        self.store().set_merge_status(
            entry_id,
            MergeStatus::Promoted,
            None,
            Some(&crate::events::merge_promoted_payload(
                &branch,
                &merge_commit,
            )),
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
    conflicts: &[String],
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
            .map(|c| format!("- {c}"))
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
