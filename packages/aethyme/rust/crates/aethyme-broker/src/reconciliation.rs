//! Conservative reconciliation of the local promoted integration layer
//! with an externally advanced upstream ref.
//!
//! This module deliberately does not fetch. Operators decide when remote
//! state is current; reconciliation only inspects the named local ref.

use std::collections::BTreeMap;

use crate::broker::{Broker, BrokerOpError};
use crate::store::ReconciliationQueueUpdate;
use crate::types::{MergeQueueEntry, MergeStatus};

#[derive(Debug, Clone)]
pub struct IntegrationReconcileOptions {
    pub upstream: String,
    pub apply: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationReconcileClassification {
    AlreadyLanded,
    SupersededUpstream,
    StillPending,
    GenuinelyConflicting,
    Ambiguous,
}

impl IntegrationReconcileClassification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyLanded => "already_landed",
            Self::SupersededUpstream => "superseded_upstream",
            Self::StillPending => "still_pending",
            Self::GenuinelyConflicting => "genuinely_conflicting",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationReconcileEntry {
    pub queue_entry_id: i64,
    pub session_id: i64,
    pub classification: IntegrationReconcileClassification,
    pub old_merge_commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_landing: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replayed_commit: Option<String>,
    pub files: Vec<String>,
    pub conflicts: Vec<String>,
    pub evidence: String,
}

#[derive(Debug, serde::Serialize)]
pub struct IntegrationReconcileReport {
    pub branch: String,
    pub upstream_ref: String,
    pub local_main: String,
    pub upstream_head: String,
    pub old_integration: String,
    pub new_integration: String,
    pub safe: bool,
    pub applied: bool,
    pub entries: Vec<IntegrationReconcileEntry>,
    pub warnings: Vec<String>,
    pub next_action: String,
}

#[derive(Clone)]
struct Candidate {
    entry: MergeQueueEntry,
    merge_commit: String,
    old_parent: String,
    files: Vec<String>,
}

impl Broker {
    /// Complete or cancel an interrupted two-phase reconciliation. Called
    /// on every broker open before any command can observe queue/ref drift.
    pub(crate) fn recover_prepared_reconciliation(&mut self) -> Result<(), BrokerOpError> {
        let Some(prepared) = self.store().prepared_integration_reconciliation()? else {
            return Ok(());
        };
        let actual = self
            .repo_handle()
            .resolve_ref(&prepared.branch)
            .unwrap_or_else(|| "<missing>".into());
        if actual == prepared.new_integration {
            self.store().finalize_integration_reconciliation()?;
            return Ok(());
        }
        if actual == prepared.old_integration {
            self.store().abort_integration_reconciliation()?;
            return Ok(());
        }
        Err(BrokerOpError::ReconciliationRecoveryRequired {
            branch: prepared.branch,
            actual,
            old: prepared.old_integration,
            new: prepared.new_integration,
        })
    }

    /// Inspect a previously fetched upstream ref and, when `apply` is set,
    /// atomically replace the integration layer plus its durable queue
    /// description. Ambiguity and content conflicts are reportable plans,
    /// never partial mutations.
    pub fn reconcile_integration(
        &mut self,
        options: IntegrationReconcileOptions,
    ) -> Result<IntegrationReconcileReport, BrokerOpError> {
        self.recover_prepared_reconciliation()?;
        let upstream_head = self
            .repo_handle()
            .resolve_ref(&options.upstream)
            .ok_or_else(|| BrokerOpError::UpstreamRefNotFound {
                upstream: options.upstream.clone(),
            })?;
        let local_main = self.repo_handle().head_commit()?;
        let (branch, old_integration) = self.integration_head()?;
        let mut report = IntegrationReconcileReport {
            branch: branch.clone(),
            upstream_ref: options.upstream.clone(),
            local_main: local_main.clone(),
            upstream_head: upstream_head.clone(),
            old_integration: old_integration.clone(),
            new_integration: upstream_head.clone(),
            safe: true,
            applied: false,
            entries: Vec::new(),
            warnings: Vec::new(),
            next_action: String::new(),
        };

        if !self.repo_handle().is_ancestor(&local_main, &upstream_head) {
            report.safe = false;
            report.warnings.push(format!(
                "local main {local_main} is not an ancestor of {} {upstream_head}; refusing to choose between divergent histories",
                options.upstream
            ));
            report.next_action =
                "inspect the main/upstream divergence, update the local main checkout, then rerun the dry-run"
                    .into();
            return Ok(report);
        }

        let queue = self.store().merge_queue()?;
        let mut candidates = Vec::new();
        for entry in queue
            .into_iter()
            .filter(|entry| entry.status == MergeStatus::Promoted)
        {
            let Some(merge_commit) = promoted_commit(&entry) else {
                continue;
            };
            if !self
                .repo_handle()
                .is_ancestor(&merge_commit, &old_integration)
            {
                continue;
            }
            let old_parent = self.repo_handle().first_parent(&merge_commit)?;
            let files = self
                .repo_handle()
                .changed_between(&old_parent, &merge_commit)?;
            candidates.push(Candidate {
                entry,
                merge_commit,
                old_parent,
                files,
            });
        }

        // Every commit in the local pending layer must be explained by the
        // promoted queue. Otherwise rebuilding from only known entries could
        // silently discard an operator-created or partially recorded commit.
        let described_chain_is_complete = if candidates.is_empty() {
            self.repo_handle()
                .is_ancestor(&old_integration, &upstream_head)
        } else {
            self.repo_handle()
                .is_ancestor(&candidates[0].old_parent, &upstream_head)
                && candidates
                    .windows(2)
                    .all(|pair| pair[1].old_parent == pair[0].merge_commit)
                && candidates
                    .last()
                    .is_some_and(|candidate| candidate.merge_commit == old_integration)
        };
        if !described_chain_is_complete {
            report.safe = false;
            report.warnings.push(
                "integration contains commits that are not a contiguous promoted queue layer; refusing to rebuild because unrecorded work could be lost"
                    .into(),
            );
            report.next_action =
                "inspect integration history and broker queue records; no refs or broker rows were changed"
                    .into();
            return Ok(report);
        }

        let upstream_commits = self
            .repo_handle()
            .commits_between_oldest(&local_main, &upstream_head)?;
        let mut upstream_by_patch: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for commit in &upstream_commits {
            let parent = self.repo_handle().first_parent(commit)?;
            if let Some(patch_id) = self.repo_handle().patch_id_between(&parent, commit)? {
                upstream_by_patch
                    .entry(patch_id)
                    .or_default()
                    .push(commit.clone());
            }
        }

        let mut classified: Vec<Option<IntegrationReconcileEntry>> = vec![None; candidates.len()];

        // Exact graph ancestry is conclusive and takes precedence over
        // content matching.
        for (index, candidate) in candidates.iter().enumerate() {
            if self
                .repo_handle()
                .is_ancestor(&candidate.merge_commit, &upstream_head)
            {
                classified[index] = Some(entry_report(
                    candidate,
                    IntegrationReconcileClassification::AlreadyLanded,
                    Some(candidate.merge_commit.clone()),
                    None,
                    Vec::new(),
                    "promoted merge commit is reachable from upstream".into(),
                ));
            }
        }

        // Match largest contiguous groups first. This recognizes one
        // upstream squash commit that contains several promoted entries.
        for group_len in (1..=candidates.len()).rev() {
            for start in 0..=candidates.len().saturating_sub(group_len) {
                let end = start + group_len;
                if classified[start..end].iter().any(Option::is_some) {
                    continue;
                }
                let Some(patch_id) = self.repo_handle().patch_id_between(
                    &candidates[start].old_parent,
                    &candidates[end - 1].merge_commit,
                )?
                else {
                    continue;
                };
                let Some(matches) = upstream_by_patch.get(&patch_id) else {
                    continue;
                };
                if matches.len() > 1 {
                    for index in start..end {
                        classified[index] = Some(entry_report(
                            &candidates[index],
                            IntegrationReconcileClassification::Ambiguous,
                            None,
                            None,
                            Vec::new(),
                            format!(
                                "stable patch id matches multiple upstream commits: {}",
                                matches.join(", ")
                            ),
                        ));
                    }
                    continue;
                }
                let landing = matches[0].clone();
                for index in start..end {
                    classified[index] = Some(entry_report(
                        &candidates[index],
                        IntegrationReconcileClassification::AlreadyLanded,
                        Some(landing.clone()),
                        None,
                        Vec::new(),
                        if group_len == 1 {
                            "stable patch id matches upstream commit".into()
                        } else {
                            format!(
                                "stable cumulative patch id matches one upstream squash for {group_len} promoted entries"
                            )
                        },
                    ));
                }
            }
        }

        // Identical final content on every path touched by an unmatched
        // promotion is sufficient to call it superseded, but empty deltas
        // are ambiguous because they provide no content evidence.
        for (index, candidate) in candidates.iter().enumerate() {
            if classified[index].is_some() || candidate.files.is_empty() {
                continue;
            }
            if self.repo_handle().paths_equal(
                &candidate.merge_commit,
                &upstream_head,
                &candidate.files,
            )? {
                classified[index] = Some(entry_report(
                    candidate,
                    IntegrationReconcileClassification::SupersededUpstream,
                    Some(upstream_head.clone()),
                    None,
                    Vec::new(),
                    "upstream has identical content on every path changed by the promotion".into(),
                ));
            }
        }

        let mut rebuilt = upstream_head.clone();
        let mut replay_blocked = false;
        for (index, candidate) in candidates.iter().enumerate() {
            if let Some(existing) = classified[index].as_ref() {
                if matches!(
                    existing.classification,
                    IntegrationReconcileClassification::Ambiguous
                        | IntegrationReconcileClassification::GenuinelyConflicting
                ) {
                    replay_blocked = true;
                }
                continue;
            }
            if candidate.files.is_empty() || replay_blocked {
                classified[index] = Some(entry_report(
                    candidate,
                    IntegrationReconcileClassification::Ambiguous,
                    None,
                    None,
                    Vec::new(),
                    if replay_blocked {
                        "cannot classify safely after an earlier unresolved queue entry".into()
                    } else {
                        "promotion has no changed-path evidence".into()
                    },
                ));
                replay_blocked = true;
                continue;
            }
            let simulation = self.repo_handle().merge_tree_with_base(
                &candidate.old_parent,
                &rebuilt,
                &candidate.merge_commit,
            )?;
            if !simulation.conflicts.is_empty() {
                classified[index] = Some(entry_report(
                    candidate,
                    IntegrationReconcileClassification::GenuinelyConflicting,
                    None,
                    None,
                    simulation.conflicts,
                    "promoted delta conflicts when replayed onto current upstream".into(),
                ));
                replay_blocked = true;
                continue;
            }
            let replayed = self.repo_handle().commit_tree(
                &simulation.tree,
                &[&rebuilt],
                &format!(
                    "broker: reconcile pending queue entry {} (session {})",
                    candidate.entry.id, candidate.entry.session_id
                ),
            )?;
            rebuilt = replayed.clone();
            classified[index] = Some(entry_report(
                candidate,
                IntegrationReconcileClassification::StillPending,
                None,
                Some(replayed),
                Vec::new(),
                "promoted delta is absent upstream and replays cleanly".into(),
            ));
        }

        report.entries = classified.into_iter().flatten().collect();
        report.safe = report.entries.iter().all(|entry| {
            !matches!(
                entry.classification,
                IntegrationReconcileClassification::Ambiguous
                    | IntegrationReconcileClassification::GenuinelyConflicting
            )
        });
        report.new_integration = rebuilt;

        if !report.safe {
            report.next_action =
                "resolve the ambiguous/conflicting entries manually; no refs or broker rows were changed"
                    .into();
            return Ok(report);
        }
        if !options.apply {
            report.next_action = format!(
                "review this dry-run, then run `aethyme broker integration reconcile --upstream {} --apply`",
                options.upstream
            );
            return Ok(report);
        }

        let updates = reconciliation_updates(&branch, &options.upstream, &report.entries);
        self.store().prepare_integration_reconciliation(
            &branch,
            &options.upstream,
            &local_main,
            &old_integration,
            &upstream_head,
            &report.new_integration,
            &updates,
        )?;
        if let Err(ref_error) = self.repo_handle().update_branch_ref_checked(
            &branch,
            &report.new_integration,
            &old_integration,
        ) {
            self.store().abort_integration_reconciliation()?;
            return Err(ref_error.into());
        }
        if let Err(store_error) = self.store().finalize_integration_reconciliation() {
            if let Err(rollback_error) = self.repo_handle().update_branch_ref_checked(
                &branch,
                &old_integration,
                &report.new_integration,
            ) {
                return Err(BrokerOpError::ReconciliationRollbackFailed {
                    reason: format!(
                        "database error: {store_error}; ref rollback error: {rollback_error}"
                    ),
                });
            }
            self.store().abort_integration_reconciliation()?;
            return Err(store_error.into());
        }
        report.applied = true;
        report.next_action =
            "integration and broker queue reconciled; submit new session work normally".into();
        Ok(report)
    }
}

fn promoted_commit(entry: &MergeQueueEntry) -> Option<String> {
    let details: serde_json::Value = serde_json::from_str(entry.details_json.as_deref()?).ok()?;
    details.get("commit")?.as_str().map(str::to_string)
}

fn entry_report(
    candidate: &Candidate,
    classification: IntegrationReconcileClassification,
    upstream_landing: Option<String>,
    replayed_commit: Option<String>,
    conflicts: Vec<String>,
    evidence: String,
) -> IntegrationReconcileEntry {
    IntegrationReconcileEntry {
        queue_entry_id: candidate.entry.id,
        session_id: candidate.entry.session_id,
        classification,
        old_merge_commit: candidate.merge_commit.clone(),
        upstream_landing,
        replayed_commit,
        files: candidate.files.clone(),
        conflicts,
        evidence,
    }
}

fn reconciliation_updates(
    branch: &str,
    upstream_ref: &str,
    entries: &[IntegrationReconcileEntry],
) -> Vec<ReconciliationQueueUpdate> {
    entries
        .iter()
        .filter_map(|entry| {
            let (status, details, replayed_commit) = match entry.classification {
                IntegrationReconcileClassification::AlreadyLanded
                | IntegrationReconcileClassification::SupersededUpstream => {
                    let details = crate::events::merge_externally_landed_payload(
                        branch,
                        &entry.old_merge_commit,
                        entry.classification.as_str(),
                        upstream_ref,
                        entry.upstream_landing.as_deref(),
                    );
                    (MergeStatus::ExternallyLanded, details, None)
                }
                IntegrationReconcileClassification::StillPending => {
                    let replayed = entry.replayed_commit.clone()?;
                    let details = serde_json::json!({
                        "branch": branch,
                        "commit": replayed,
                        "reconciled_from": entry.old_merge_commit,
                        "upstream_ref": upstream_ref,
                    })
                    .to_string();
                    (MergeStatus::Promoted, details, Some(replayed))
                }
                IntegrationReconcileClassification::GenuinelyConflicting
                | IntegrationReconcileClassification::Ambiguous => return None,
            };
            Some(ReconciliationQueueUpdate {
                queue_entry_id: entry.queue_entry_id,
                status,
                merged_tree: None,
                details_json: details,
                classification: entry.classification.as_str().to_string(),
                old_merge_commit: entry.old_merge_commit.clone(),
                upstream_landing: entry.upstream_landing.clone(),
                replayed_commit,
            })
        })
        .collect()
}
