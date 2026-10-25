//! Bounded, advisory ownership of a failure observed on an exact fetched base.
//!
//! Local gate rows and operator-supplied test identifiers are evidence records,
//! not attestations or cache authority. Nothing here relaxes verification.

use crate::{Broker, BrokerOpError};

pub const MAX_OPEN_BASE_FAILURE_INCIDENTS: usize = 128;
pub const RETAIN_RESOLVED_BASE_FAILURE_INCIDENTS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BaseFailureIncident {
    pub id: i64,
    pub base_commit: String,
    pub tree_hash: String,
    pub gate_name: String,
    pub definition_hash: String,
    pub execution_profile_hash: String,
    pub owner_session_id: i64,
    pub repair_pr: Option<i64>,
    /// Operator-supplied identifiers; never inferred from partial logs.
    pub test_ids: Vec<String>,
    pub confirmed_run_id: i64,
    pub confirmed_run_created_at: i64,
    pub confirmed_at: i64,
    pub resolved_run_id: Option<i64>,
    pub resolved_base_commit: Option<String>,
    pub resolved_at: Option<i64>,
}

pub(crate) fn invalid(reason: impl Into<String>) -> crate::BrokerError {
    crate::BrokerError::InvalidBaseFailureIncident(reason.into())
}

pub(crate) fn normalize_test_ids(ids: &[String]) -> Result<Vec<String>, crate::BrokerError> {
    if ids.is_empty() || ids.len() > 64 {
        return Err(invalid("supply 1..=64 failing test identifiers"));
    }
    let mut ids = ids.to_vec();
    for id in &ids {
        if id.is_empty() || id.len() > 200 || id.trim() != id || id.chars().any(char::is_control) {
            return Err(invalid(
                "test identifiers must be 1..=200 bytes without surrounding whitespace or control characters",
            ));
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

impl Broker {
    /// Confirm one exact fetched-base failure and assign its first repair owner.
    /// Concurrent confirmation returns the existing immutable incident instead
    /// of stealing its owner or changing the declared failing tests.
    pub fn confirm_base_failure_incident(
        &mut self,
        owner_session_id: i64,
        failed_run_id: i64,
        test_ids: &[String],
        repair_pr: Option<i64>,
    ) -> Result<BaseFailureIncident, BrokerOpError> {
        let (base, tree) = self.base_failure_current_base()?;
        let proof = self.store_ref().base_incident_gate_proof(failed_run_id)?;
        let (_, gates) = crate::load_gates_at_commit(self.repo_handle(), &base)
            .map_err(|error| invalid(error.to_string()))?;
        if !gates.iter().any(|gate| {
            gate.name == proof.gate_name && gate.definition_hash == proof.definition_hash
        }) {
            return Err(
                invalid("recorded gate definition is not the committed base definition").into(),
            );
        }
        self.store()
            .confirm_base_incident(
                owner_session_id,
                failed_run_id,
                &base,
                &tree,
                test_ids,
                repair_pr,
            )
            .map_err(Into::into)
    }

    /// Resolve only with a newer recorded passing run on the current fetched
    /// default tree, matching the incident's gate, definition and exact profile.
    /// A passing feature candidate does not establish that the base is green.
    pub fn resolve_base_failure_incident(
        &mut self,
        incident_id: i64,
        passing_run_id: i64,
    ) -> Result<BaseFailureIncident, BrokerOpError> {
        let (base, tree) = self.base_failure_current_base()?;
        self.store()
            .resolve_base_incident(incident_id, passing_run_id, &base, &tree)
            .map_err(Into::into)
    }

    /// Bounded registry read; does not inspect historical worktrees or processes.
    pub fn base_failure_incidents(&self) -> Result<Vec<BaseFailureIncident>, BrokerOpError> {
        self.store_ref().base_incidents().map_err(Into::into)
    }

    fn base_failure_current_base(&self) -> Result<(String, String), BrokerOpError> {
        // Require an actual default remote HEAD, never current-branch upstream
        // fallback. This observation is offline and only as fresh as last fetch.
        let reference = self
            .repo_handle()
            .symbolic_ref("refs/remotes/origin/HEAD")
            .filter(|value| {
                value.starts_with("refs/remotes/origin/") && value != "refs/remotes/origin/HEAD"
            })
            .ok_or_else(|| invalid("origin/HEAD must identify the fetched default branch"))?;
        let commit = self
            .repo_handle()
            .resolve_ref(&reference)
            .ok_or_else(|| invalid("fetched default branch does not resolve"))?;
        let tree = self.repo_handle().commit_tree_id(&commit)?;
        Ok((commit, tree))
    }
}

impl Broker {
    pub(crate) fn base_failure_incident_status_advice(
        &self,
        agents: &[crate::AgentView],
    ) -> Vec<crate::StatusAdvice> {
        let records = match self.base_failure_incidents() {
            Ok(records) => records
                .into_iter()
                .filter(|r| r.resolved_at.is_none())
                .collect::<Vec<_>>(),
            Err(error) => {
                return vec![crate::StatusAdvice {
                    id: "base.incidents-unavailable",
                    severity: crate::StatusAdviceSeverity::Warning,
                    reason: "base_failure_registry_unavailable",
                    summary: format!("Base-failure registry could not be read: {error}"),
                    session_id: None,
                    queue_entry_id: None,
                    evidence: Vec::new(),
                    commands: vec!["aethyme broker advanced gates incidents list --json".into()],
                }];
            }
        };
        if records.is_empty() {
            return Vec::new(); // No additional Git work for an empty registry.
        }
        let current_base = self
            .base_failure_current_base()
            .ok()
            .map(|(commit, _)| commit);
        let mut advice = records.iter().take(8).map(|record| {
            let base_matches = current_base.as_deref() == Some(record.base_commit.as_str());
            let owner_present = agents.iter().any(|a| a.session.id == record.owner_session_id);
            crate::StatusAdvice {
                id: "base.failure-incident", severity: if base_matches { crate::StatusAdviceSeverity::Warning } else { crate::StatusAdviceSeverity::Notice },
                reason: if base_matches { "recorded_failure_on_fetched_base" } else { "prior_base_failure_not_requalified" },
                summary: format!("Incident {}: {} failed on recorded base {}; repair owner session {}{}. {}",
                    record.id, record.gate_name, record.base_commit, record.owner_session_id,
                    if owner_present { "" } else { " (absent from current session inventory)" },
                    if base_matches { "Fetched base matches; compare the recorded profile before diagnosing again. Gates still run." }
                    else { "Current base is different or unknown; this failure has not been requalified. Gates still run." }),
                session_id: Some(record.owner_session_id), queue_entry_id: None,
                evidence: vec![format!("confirmed_run={} profile={} repair_pr={:?}", record.confirmed_run_id, record.execution_profile_hash, record.repair_pr),
                    format!("declared_test_count={}", record.test_ids.len())],
                commands: vec!["aethyme broker advanced gates incidents list --json".into()],
            }
        }).collect::<Vec<_>>();
        if records.len() > 8 {
            advice.push(crate::StatusAdvice {
                id: "base.incidents-omitted",
                severity: crate::StatusAdviceSeverity::Notice,
                reason: "bounded_base_failure_advice",
                summary: format!(
                    "{} additional unresolved base-failure incidents; inspect the bounded registry",
                    records.len() - 8
                ),
                session_id: None,
                queue_entry_id: None,
                evidence: Vec::new(),
                commands: vec!["aethyme broker advanced gates incidents list --json".into()],
            });
        }
        advice
    }
}
