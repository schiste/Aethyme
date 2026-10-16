//! Exact private-completion bindings used when a later Local wave builds on
//! an accepted source. This module never makes an unverified synthesis an
//! accepted baseline; it only reconstructs the prior request and provenance
//! bindings that callers must validate before dispatch or gate verification.

use std::collections::BTreeSet;
use std::path::Path;

use aethyme_contracts::experimental_v0::{RecordId, SourceSnapshotId};
use serde_json::Value;

use crate::collaboration_archive::{self as archive, CommitOid};
use crate::collaboration_state::CollaborationStore;
use crate::local_acceptance::{HistoricalCompletionIdentity, verify_historical_completion};
use crate::local_controller::CompletionSpec;
use crate::local_target::{ControllerOwner, TargetRevision};
use crate::resolution::ResolutionRequest;

use super::{Wave, WaveError, WaveSpec, accepted_completion, id, snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevisionRelation {
    Same,
    Cross,
}

/// The accepted source that a frozen wave is allowed to build on. The
/// completion's accepted commit is the request baseline; its source snapshot
/// is the retained identity used by the Local wave and resolver request.
#[derive(Debug, Clone)]
pub(super) struct Binding {
    key: RecordId,
    completion: CompletionSpec,
    previous: WaveSpec,
    relation: RevisionRelation,
    source: SourceSnapshotId,
    accepted_commit: CommitOid,
}

pub(super) fn load(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    wave: &Wave,
    pinned: &TargetRevision,
) -> Result<Option<Binding>, WaveError> {
    let Some(raw_key) = wave.spec.prior_completion.as_deref() else {
        return Ok(None);
    };
    if wave.spec.target != owner.target()
        || wave.spec.issuer != owner.issuer()
        || pinned.target != wave.spec.target
        || pinned.issuer != wave.spec.issuer
        || pinned.id.as_str() != wave.spec.revision
    {
        return Err(WaveError::Stale);
    }

    let key = id(raw_key)?;
    let (completion, previous) = accepted_completion(store, &key, wave.spec.sequence)?;
    if completion.target != owner.target()
        || previous.target != wave.spec.target
        || previous.issuer != wave.spec.issuer
        || previous.revision != completion.revision
    {
        return Err(WaveError::Stale);
    }
    let historical = HistoricalCompletionIdentity {
        target: completion.target.clone(),
        issuer: previous.issuer.clone(),
        revision: completion.revision.clone(),
        epoch: completion.epoch.clone(),
        wave: completion.wave.clone(),
        source: completion.source.clone(),
        candidate_commit: completion.candidate_commit.clone(),
        candidate_tree: completion.candidate_tree.clone(),
        qualification: completion.qualification.clone(),
        receipt: completion.acceptance_receipt.clone(),
        accepted_commit: completion.accepted_commit.clone(),
        accepted_tree: completion.accepted_tree.clone(),
    };
    verify_historical_completion(store, &historical).map_err(|_| WaveError::Stale)?;

    let relation = classify_revision_relation(
        &completion.revision,
        &wave.spec.revision,
        &completion.source,
        &wave.spec.baseline,
        &pinned.spec.baseline,
        &previous.selected,
        &wave.spec.selected,
    )?;
    let source = snapshot(&completion.source)?;
    let accepted_commit = CommitOid::parse(&completion.accepted_commit)?;
    if completion.accepted_tree != completion.candidate_tree {
        return Err(WaveError::Invalid(
            "prior completion accepted tree differs from its candidate tree".into(),
        ));
    }

    Ok(Some(Binding {
        key,
        completion,
        previous,
        relation,
        source,
        accepted_commit,
    }))
}

fn classify_revision_relation(
    prior_revision: &str,
    current_revision: &str,
    prior_source: &str,
    current_wave_baseline: &str,
    current_revision_baseline: &str,
    prior_selected: &[String],
    current_selected: &[String],
) -> Result<RevisionRelation, WaveError> {
    if prior_revision == current_revision {
        let previous: BTreeSet<_> = prior_selected.iter().collect();
        let selected: BTreeSet<_> = current_selected.iter().collect();
        if previous.len() != prior_selected.len()
            || selected.len() != current_selected.len()
            || prior_selected
                .iter()
                .any(|member| !selected.contains(member))
            || prior_source != current_wave_baseline
        {
            return Err(WaveError::Invalid(
                "same-revision completion baseline or cumulative selection".into(),
            ));
        }
        Ok(RevisionRelation::Same)
    } else if prior_source == current_revision_baseline
        && current_wave_baseline == current_revision_baseline
    {
        // A new revision inherits the exact source snapshot, not the previous
        // revision's contribution identifiers or request accepted-list.
        Ok(RevisionRelation::Cross)
    } else {
        Err(WaveError::Invalid(
            "cross-revision completion does not match the explicit revision baseline".into(),
        ))
    }
}

impl Binding {
    pub(super) fn key(&self) -> &RecordId {
        &self.key
    }

    pub(super) fn source(&self) -> &SourceSnapshotId {
        &self.source
    }

    /// The actual request baseline is the accepted commit. WaveSpec::baseline
    /// is the source snapshot ID and must not be substituted for this commit.
    pub(super) fn request_baseline_commit(&self) -> &CommitOid {
        &self.accepted_commit
    }

    pub(super) fn previous_members(&self) -> &[String] {
        match self.relation {
            RevisionRelation::Same => &self.previous.selected,
            RevisionRelation::Cross => &[],
        }
    }

    pub(super) fn same_revision(&self) -> bool {
        self.relation == RevisionRelation::Same
    }

    /// The original pinned revision baseline is carried as accepted history
    /// only within the same revision. Across revisions, the accepted source
    /// itself is the explicit baseline and prior contribution IDs are not
    /// inherited.
    pub(super) fn accepted_history(
        &self,
        store: &CollaborationStore,
        pinned: &TargetRevision,
    ) -> Result<Vec<CommitOid>, WaveError> {
        if !self.same_revision() {
            return Ok(Vec::new());
        }
        let baseline = snapshot(&pinned.spec.baseline)?;
        let retained = archive::retained(store, &baseline)?
            .ok_or(crate::local_target::TargetError::Missing)?;
        Ok(vec![retained.commit])
    }

    pub(super) fn recipe_binding(&self) -> Value {
        serde_json::json!({
            "completion": self.key.as_str(),
            "members": self.previous_members(),
        })
    }

    pub(super) fn producer_profile(&self) -> &'static str {
        super::ACCEPTED_BASELINE_TEXT_PROFILE
    }

    /// A resolver request must name both halves of this binding exactly:
    /// the accepted commit and the snapshot retained by this wave.
    pub(super) fn validate_request_baseline(
        &self,
        request: &ResolutionRequest,
        wave: &Wave,
    ) -> Result<(), WaveError> {
        if request.baseline().commit != self.accepted_commit
            || request.baseline().snapshot_id != self.source
            || self.source.as_str() != wave.spec.baseline
        {
            return Err(WaveError::Invalid(
                "resolver request baseline differs from the prior completion".into(),
            ));
        }
        Ok(())
    }

    /// Verify that the accepted commit still materializes the exact source
    /// snapshot and tree stated by the private completion.
    pub(super) fn validate_git_binding(&self, repo: &Path) -> Result<(), WaveError> {
        if archive::raw_commit_tree(repo, &self.accepted_commit)? != self.completion.accepted_tree
            || archive::snapshot_of_commit(repo, &self.accepted_commit)?.id() != self.source
        {
            return Err(WaveError::Invalid(
                "prior completion accepted commit no longer matches its source".into(),
            ));
        }
        Ok(())
    }

    /// Completion artifacts are rooted independently of a later revision.
    /// Check the completion record and accepted source before relying on them.
    pub(super) fn validate_retained_completion(
        &self,
        store: &CollaborationStore,
    ) -> Result<(), WaveError> {
        let body: Vec<u8> = store.read_connection().query_row(
            "SELECT body FROM local_controller_completions WHERE id=?1",
            [self.key.as_str()],
            |row| row.get(0),
        )?;
        let object = archive::ObjectDigest::of(&body).hex();
        for (kind, reference) in [
            ("object", object.as_str()),
            ("snapshot", self.source.as_str()),
        ] {
            let rooted: bool = store.read_connection().query_row(
                "SELECT EXISTS(SELECT 1 FROM local_controller_completion_roots WHERE completion=?1 AND kind=?2 AND reference=?3)",
                rusqlite::params![self.key.as_str(), kind, reference],
                |row| row.get(0),
            )?;
            if !rooted {
                return Err(WaveError::Invalid(format!(
                    "prior completion {kind} {reference} is no longer retained"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classified(
        prior_revision: &str,
        current_revision: &str,
        prior_source: &str,
        current_wave_baseline: &str,
        current_revision_baseline: &str,
        prior_selected: &[&str],
        current_selected: &[&str],
    ) -> Result<RevisionRelation, WaveError> {
        classify_revision_relation(
            prior_revision,
            current_revision,
            prior_source,
            current_wave_baseline,
            current_revision_baseline,
            &prior_selected
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>(),
            &current_selected
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn same_revision_keeps_cumulative_members_and_source_snapshot_binding() {
        assert_eq!(
            classified(
                "rev-a",
                "rev-a",
                "sha256:source-a",
                "sha256:source-a",
                "sha256:original-a",
                &["delivered-a"],
                &["delivered-a", "new-b"],
            )
            .unwrap(),
            RevisionRelation::Same
        );
        assert!(
            classified(
                "rev-a",
                "rev-a",
                "sha256:source-a",
                "sha256:another-source",
                "sha256:original-a",
                &["delivered-a"],
                &["delivered-a", "new-b"],
            )
            .is_err()
        );
        assert!(
            classified(
                "rev-a",
                "rev-a",
                "sha256:source-a",
                "sha256:source-a",
                "sha256:original-a",
                &["delivered-a"],
                &["new-b"],
            )
            .is_err()
        );
    }

    #[test]
    fn cross_revision_reuses_only_the_exact_pinned_source_snapshot() {
        assert_eq!(
            classified(
                "rev-a",
                "rev-b",
                "sha256:accepted-source",
                "sha256:accepted-source",
                "sha256:accepted-source",
                &["old-id"],
                &["new-id"],
            )
            .unwrap(),
            RevisionRelation::Cross
        );
        assert!(
            classified(
                "rev-a",
                "rev-b",
                "sha256:accepted-source",
                "sha256:other-source",
                "sha256:accepted-source",
                &["old-id"],
                &["new-id"],
            )
            .is_err()
        );
        assert!(
            classified(
                "rev-a",
                "rev-b",
                "sha256:accepted-source",
                "sha256:accepted-source",
                "sha256:other-source",
                &["old-id"],
                &["new-id"],
            )
            .is_err()
        );
    }

    #[test]
    fn duplicate_selection_ids_cannot_become_prior_recipe_members() {
        assert!(
            classified(
                "rev-a",
                "rev-a",
                "sha256:source-a",
                "sha256:source-a",
                "sha256:original-a",
                &["same", "same"],
                &["same", "new"],
            )
            .is_err()
        );
        assert!(
            classified(
                "rev-a",
                "rev-a",
                "sha256:source-a",
                "sha256:source-a",
                "sha256:original-a",
                &["same"],
                &["same", "same"],
            )
            .is_err()
        );
    }
}
