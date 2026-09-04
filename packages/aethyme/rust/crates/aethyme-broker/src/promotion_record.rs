//! Recovery for a promotion whose queue record was lost mid-promotion.
//!
//! A promotion does two things: it advances the integration branch and creates
//! the merge commit, then it records that fact on the queue entry. When the
//! second half does not complete, the repository is left with an integration
//! commit that no entry claims. `ship` refuses to publish any prefix containing
//! such a commit, and because the commit is permanently in integration's
//! history, that refusal blocks every later release.
//!
//! The lost record is reconstructible because the entry still stores the tree
//! its simulation produced. A promotion commit whose tree equals a non-promoted
//! entry's `merged_tree` is that entry's promotion: the tree is content-addressed,
//! so the match is not a heuristic about timing or naming.
//!
//! Deliberately not used as evidence:
//!
//! - `base_commit`, which records the base at submission and is not rewritten
//!   when simulation rebinds to a moved integration head. A promoted entry can
//!   legitimately carry a base that is not its commit's first parent.
//! - `created_at`, which only correlates.
//!
//! Recovery restores `status` and `details_json` and nothing else. Path exposure
//! is reconstructed by [`Broker::backfill_promoted_path_exposures`] on the next
//! open, exactly as it is for a promotion that recorded normally.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use crate::{Broker, BrokerOpError, MergeQueueEntry, MergeStatus};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub const PROMOTION_RECORD_PLAN_SCHEMA_VERSION: u32 = 1;

/// One integration commit that no promoted entry claims.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnrecordedPromotion {
    pub commit: String,
    pub tree: String,
    /// Entry whose recorded merge tree equals this commit's tree.
    pub entry_id: Option<i64>,
    pub session_id: Option<i64>,
    /// Status the entry currently holds; restored to `promoted`.
    pub current_status: Option<String>,
    pub evidence: Vec<String>,
    /// Why this commit cannot be recovered, when `entry_id` is `None`.
    pub blocker: Option<String>,
}

impl UnrecordedPromotion {
    pub fn recoverable(&self) -> bool {
        self.entry_id.is_some() && self.blocker.is_none()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PromotionRecordPlan {
    pub schema_version: u32,
    pub digest: String,
    pub evaluated_at: i64,
    pub integration_ref: String,
    pub integration_tip: String,
    pub remote_default: Option<String>,
    pub candidates: Vec<UnrecordedPromotion>,
}

impl PromotionRecordPlan {
    pub fn recoverable(&self) -> impl Iterator<Item = &UnrecordedPromotion> {
        self.candidates.iter().filter(|c| c.recoverable())
    }

    pub fn finish_digest(&mut self) -> Result<(), serde_json::Error> {
        #[derive(serde::Serialize)]
        struct Authorization<'a> {
            schema_version: u32,
            integration_tip: &'a str,
            candidates: &'a [UnrecordedPromotion],
        }
        let bytes = serde_json::to_vec(&Authorization {
            schema_version: self.schema_version,
            integration_tip: &self.integration_tip,
            candidates: &self.candidates,
        })?;
        self.digest = format!("{:x}", Sha256::digest(bytes));
        Ok(())
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PromotionRecordApplyReport {
    pub digest: String,
    pub restored: Vec<i64>,
    pub skipped: Vec<String>,
}

/// The commit detail a normally-recorded promotion writes.
fn promotion_details(integration_ref: &str, commit: &str) -> String {
    serde_json::json!({ "branch": integration_ref, "commit": commit }).to_string()
}

fn recorded_commit(entry: &MergeQueueEntry) -> Option<String> {
    entry
        .details_json
        .as_deref()
        .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .and_then(|d| d.get("commit")?.as_str().map(str::to_string))
}

impl Broker {
    /// Read-only plan: integration commits with no promoted entry claiming them.
    pub fn promotion_record_plan(&mut self) -> Result<PromotionRecordPlan, BrokerOpError> {
        let integration_ref = crate::merge::PromoteConfig::load(&self.main_root_path()).branch;
        let tip = self
            .integration_tip()
            .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                what: "integration tip",
                reason: "integration branch does not resolve".into(),
            })?;
        let base = self
            .repo_handle()
            .tracking_upstream()
            .map(|(_, head)| head)
            .or_else(|| self.repo_handle().head_commit().ok());

        let entries = self.store().merge_queue()?;
        let claimed: BTreeSet<String> = entries
            .iter()
            .filter(|e| e.status == MergeStatus::Promoted)
            .filter_map(recorded_commit)
            .collect();

        // A tree may only be offered by one entry; ambiguity must refuse.
        let mut by_tree: BTreeMap<String, Vec<&MergeQueueEntry>> = BTreeMap::new();
        for entry in &entries {
            if entry.status == MergeStatus::Promoted {
                continue;
            }
            if let Some(tree) = entry.merged_tree.as_deref() {
                by_tree.entry(tree.to_string()).or_default().push(entry);
            }
        }

        let walk = match &base {
            Some(base) => self
                .repo_handle()
                .first_parent_commits_between_oldest(base, &tip)
                .unwrap_or_default(),
            None => Vec::new(),
        };

        let mut candidates = Vec::new();
        for commit in walk {
            if claimed.contains(&commit) {
                continue;
            }
            let tree = match self.repo_handle().commit_tree_id(&commit) {
                Ok(tree) => tree,
                Err(error) => {
                    candidates.push(UnrecordedPromotion {
                        commit,
                        tree: String::new(),
                        entry_id: None,
                        session_id: None,
                        current_status: None,
                        evidence: Vec::new(),
                        blocker: Some(format!("cannot read commit tree: {error}")),
                    });
                    continue;
                }
            };
            let matches = by_tree.get(&tree).cloned().unwrap_or_default();
            let (entry_id, session_id, current_status, evidence, blocker) = match matches.as_slice()
            {
                [] => (
                    None,
                    None,
                    None,
                    Vec::new(),
                    Some(
                        "no queue entry records a merge tree equal to this commit's tree; \
                         the commit was not produced by a recorded submission"
                            .into(),
                    ),
                ),
                [entry] => {
                    let mut evidence = vec![
                        format!("commit tree {tree} equals entry {} merged_tree", entry.id),
                        format!(
                            "entry {} is {} and claims no promotion commit",
                            entry.id,
                            entry.status.as_str()
                        ),
                    ];
                    // Corroboration only; never required, because a rebound base
                    // legitimately differs from the commit's first parent.
                    if let Ok(parent) = self.repo_handle().first_parent(&commit)
                        && parent == entry.base_commit
                    {
                        evidence.push(format!("commit first parent {parent} equals recorded base"));
                    }
                    (
                        Some(entry.id),
                        Some(entry.session_id),
                        Some(entry.status.as_str().to_string()),
                        evidence,
                        None,
                    )
                }
                many => (
                    None,
                    None,
                    None,
                    Vec::new(),
                    Some(format!(
                        "{} entries record this tree ({}); ambiguous provenance is never repaired",
                        many.len(),
                        many.iter()
                            .map(|e| e.id.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                ),
            };
            candidates.push(UnrecordedPromotion {
                commit,
                tree,
                entry_id,
                session_id,
                current_status,
                evidence,
                blocker,
            });
        }

        let mut plan = PromotionRecordPlan {
            schema_version: PROMOTION_RECORD_PLAN_SCHEMA_VERSION,
            digest: String::new(),
            evaluated_at: now_ms(),
            integration_ref,
            integration_tip: tip,
            remote_default: base,
            candidates,
        };
        plan.finish_digest()?;
        Ok(plan)
    }

    /// Restore the promoted record for every recoverable candidate in an
    /// operator-confirmed plan. Every precondition is re-proved first.
    pub fn promotion_record_apply(
        &mut self,
        confirm: &str,
    ) -> Result<PromotionRecordApplyReport, BrokerOpError> {
        let plan = self.promotion_record_plan()?;
        if !plan.digest.eq_ignore_ascii_case(confirm) {
            return Err(BrokerOpError::PromotionRecordConfirmationMismatch {
                expected: plan.digest,
                actual: confirm.to_owned(),
            });
        }
        let mut report = PromotionRecordApplyReport {
            digest: plan.digest.clone(),
            ..Default::default()
        };
        for candidate in &plan.candidates {
            let (Some(entry_id), None) = (candidate.entry_id, candidate.blocker.as_ref()) else {
                if let Some(blocker) = &candidate.blocker {
                    report
                        .skipped
                        .push(format!("{}: {blocker}", candidate.commit));
                }
                continue;
            };
            let entry = self.queue_entry(entry_id)?;
            if entry.status == MergeStatus::Promoted {
                report
                    .skipped
                    .push(format!("entry {entry_id} is already promoted"));
                continue;
            }
            if entry.merged_tree.as_deref() != Some(candidate.tree.as_str()) {
                report.skipped.push(format!(
                    "entry {entry_id} merge tree changed since the plan was reviewed"
                ));
                continue;
            }
            let details = promotion_details(&plan.integration_ref, &candidate.commit);
            self.store().set_merge_status(
                entry_id,
                MergeStatus::Promoted,
                entry.merged_tree.as_deref(),
                Some(&details),
            )?;
            report.restored.push(entry_id);
        }
        if !report.restored.is_empty() {
            let payload = serde_json::json!({
                "digest": report.digest,
                "restored": report.restored,
            })
            .to_string();
            self.store().append_event(
                crate::events::BROKER_PROMOTION_RECORD_RESTORED,
                None,
                Some(&payload),
            )?;
            // Rebuild the path exposures the lost promotions never recorded.
            self.backfill_promoted_path_exposures()?;
        }
        Ok(report)
    }
}
