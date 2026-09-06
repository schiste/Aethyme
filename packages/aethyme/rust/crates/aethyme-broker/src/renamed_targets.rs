//! Telling a session that its target paths moved under it (issue #145).
//!
//! When an already-promoted session renames a file, a branch still targeting the
//! old name fails to replay with `CONFLICT (modify/delete): ... deleted in HEAD`
//! -- indistinguishable from the file genuinely having been deleted. Finding the
//! truth means running `git log --follow` by hand and tracing the rename to the
//! commit that made it.
//!
//! The broker can answer this directly: it knows the session's baseline, the
//! paths its commits touch, and which promoted entry introduced each path.

use crate::{Broker, BrokerOpError};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RenamedTarget {
    /// The path this session's commits still modify.
    pub from: String,
    /// Where that content lives on the integration tip.
    pub to: String,
    /// The promoted queue entry that introduced the new path, when one owns it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promoted_entry_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promoted_session_id: Option<i64>,
}

impl Broker {
    /// Paths this session's commits touch that no longer exist on the
    /// integration tip but were renamed there by a later promotion.
    ///
    /// Deliberately silent about paths that were genuinely deleted: a deletion
    /// is a real conflict the operator must resolve, and reporting it as a
    /// rename would send them to a file that does not exist.
    pub fn session_renamed_targets(
        &mut self,
        session_id: i64,
    ) -> Result<Vec<RenamedTarget>, BrokerOpError> {
        let session = self.store().session(session_id)?;
        let Some(baseline) = session
            .adoption_base
            .clone()
            .or_else(|| session.diff_base.clone())
        else {
            return Ok(Vec::new());
        };
        let Some(integration_tip) = self.integration_tip() else {
            return Ok(Vec::new());
        };

        let head = match self.repo_handle().resolve_ref(&session.branch) {
            Some(head) => head,
            None => return Ok(Vec::new()),
        };

        // Bounded to what this session actually changed, not a whole-tree scan.
        let repo = self.repo_handle();
        let mut touched: Vec<String> = Vec::new();
        for commit in repo
            .commits_between_oldest(&baseline, &head)
            .unwrap_or_default()
        {
            for (_status, path) in repo.commit_changed_paths(&commit).unwrap_or_default() {
                if !touched.contains(&path) {
                    touched.push(path);
                }
            }
        }
        touched.retain(|path| repo.blob_at(&integration_tip, path).is_none());
        if touched.is_empty() {
            return Ok(Vec::new());
        }

        let renames = repo
            .renames_between(&baseline, &integration_tip)
            .unwrap_or_default();
        let mut found = Vec::new();
        for path in touched {
            let Some((_, new_path)) = renames.iter().find(|(old, _)| old == &path) else {
                // Absent and not renamed: a genuine deletion, which is the
                // operator's conflict to resolve, not ours to reinterpret.
                continue;
            };
            let owner = self.promoted_entry_introducing(new_path);
            found.push(RenamedTarget {
                from: path,
                to: new_path.clone(),
                promoted_entry_id: owner.map(|(id, _)| id),
                promoted_session_id: owner.map(|(_, session)| session),
            });
        }
        Ok(found)
    }

    /// The earliest promoted entry whose commit already contains `path`.
    ///
    /// Presence is monotonic once a path is added, so this binary-searches the
    /// promoted entries instead of testing each one -- roughly ten lookups on a
    /// queue of a thousand rather than a thousand.
    fn promoted_entry_introducing(&mut self, path: &str) -> Option<(i64, i64)> {
        let mut promoted: Vec<(i64, i64, String)> = self
            .store()
            .merge_queue()
            .ok()?
            .into_iter()
            .filter(|entry| entry.status == crate::MergeStatus::Promoted)
            .filter_map(|entry| {
                let details =
                    serde_json::from_str::<serde_json::Value>(entry.details_json.as_deref()?)
                        .ok()?;
                let commit = details.get("commit")?.as_str()?.to_string();
                Some((entry.id, entry.session_id, commit))
            })
            .collect();
        promoted.sort_by_key(|(id, _, _)| *id);

        let repo = self.repo_handle();
        let present = |commit: &str| repo.blob_at(commit, path).is_some();
        if promoted.is_empty() || !present(&promoted[promoted.len() - 1].2) {
            return None;
        }
        let (mut low, mut high) = (0usize, promoted.len() - 1);
        while low < high {
            let mid = (low + high) / 2;
            if present(&promoted[mid].2) {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        let (entry_id, session_id, _) = &promoted[low];
        Some((*entry_id, *session_id))
    }
}
