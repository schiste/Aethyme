//! Reclaiming regenerable build artefacts from session worktrees.
//!
//! Every session worktree keeps its own Cargo `target/` (and friends), several
//! gigabytes each, and nothing removes them when the session is done. Measured
//! on one developer machine: 52 GiB in one repository's worktrees, 4.9 GiB in
//! another's. The disk then fills, and a full disk does not present as a disk
//! problem -- it presents as link errors and unrelated test failures whose
//! verdict is cached (#156).
//!
//! Deletion is not automatic. A retained worktree may be retained precisely
//! because someone intends to resume in it, and re-running a cold build is a
//! real cost. So this reports candidates and only removes what an operator
//! reviewed, following the same digest-bound plan/apply contract as `gc`.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

const RECLAIM_PLAN_SCHEMA_VERSION: u8 = 1;
const RECLAIM_PLAN_FILENAME_PREFIX: &str = ".aethyme-reclaim-plan-";
const RECLAIM_PLAN_FILENAME_SUFFIX: &str = ".json";
/// Recent whole-root measurements, so a second plan reuses them instead of
/// walking the same build trees again. Named like a review snapshot so root
/// inventories treat it as broker bookkeeping; it is never a valid digest.
const RECLAIM_SIZE_CACHE_FILENAME: &str = ".aethyme-reclaim-plan-sizes.json";
/// Present while a whole-root plan is running, naming its process.
const RECLAIM_IN_FLIGHT_FILENAME: &str = ".aethyme-reclaim-plan-in-flight.json";
const RECLAIM_SIZE_CACHE_SCHEMA_VERSION: u8 = 1;

/// The part of a candidate that determines whether an apply would touch it.
/// Measured bytes deliberately do not belong here: a build may grow while the
/// operator is reviewing the same deletion decision.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReclaimDecision {
    pub path: PathBuf,
    pub reclaimable: bool,
}

/// Return the stable, decision-only view of a scan.
pub fn decisions(candidates: &[ReclaimCandidate]) -> Vec<ReclaimDecision> {
    let mut decisions = candidates
        .iter()
        .map(|candidate| ReclaimDecision {
            path: candidate.path.clone(),
            reclaimable: candidate.reclaimable,
        })
        .collect::<Vec<_>>();
    decisions.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.reclaimable.cmp(&right.reclaimable))
    });
    decisions
}

fn decision_digest(root: &Path, scope: Option<&Path>, decisions: &[ReclaimDecision]) -> String {
    decision_digest_of(root, scope, false, decisions)
}

fn decision_digest_of(
    root: &Path,
    scope: Option<&Path>,
    partial: bool,
    decisions: &[ReclaimDecision],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aethyme-reclaim-plan-v1\0");
    hasher.update(root.as_os_str().as_encoded_bytes());
    // A plan whose discovery stopped early lists only part of the root. Its
    // digest can never equal a complete scan's, so no confirmation of it
    // reads as a review of the whole root.
    if partial {
        hasher.update(b"\0partial\0");
    }
    // A whole-root plan hashes as it always has, so its saved reviews stay
    // valid; a one-worktree plan also commits to which worktree it covers.
    if let Some(scope) = scope {
        hasher.update(b"\0scope\0");
        hasher.update(scope.as_os_str().as_encoded_bytes());
    }
    for decision in decisions {
        hasher.update(b"\n");
        hasher.update(decision.path.as_os_str().as_encoded_bytes());
        hasher.update(if decision.reclaimable {
            &b"\0reclaimable"[..]
        } else {
            &b"\0kept"[..]
        });
    }
    format!("{:x}", hasher.finalize())
}

/// Digest over exactly the deletion decision: the sorted candidate paths and
/// whether each is reclaimable. A candidate's measured size is displayed in a
/// plan but is not authorization-bearing, so an active build can grow without
/// invalidating an otherwise unchanged review.
///
/// `scope` is the one worktree a session-scoped plan covers. It is part of
/// what the digest authorizes: such a plan was scanned from that worktree
/// alone, so it says nothing about the rest of the root, and a whole-root
/// plan was not reviewed as being about one session.
pub fn plan_digest(root: &Path, scope: Option<&Path>, candidates: &[ReclaimCandidate]) -> String {
    decision_digest(root, scope, &decisions(candidates))
}

/// Digest for a whole-root plan whose discovery stopped before every worktree
/// was walked. It is displayed for reference but never saved as a review, and
/// it cannot match any complete scan, so apply refuses it.
pub fn partial_plan_digest(root: &Path, candidates: &[ReclaimCandidate]) -> String {
    decision_digest_of(root, None, true, &decisions(candidates))
}

/// A fresh scan narrowed to what a review authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewedScope {
    /// The fresh candidates, with every one the review did not mark
    /// reclaimable demoted to kept.
    pub candidates: Vec<ReclaimCandidate>,
    /// Reviewed-reclaimable paths this apply will not remove, and why.
    pub withdrawn: Vec<String>,
    /// Reclaimable in the fresh scan but absent from the review.
    pub not_reviewed: Vec<PathBuf>,
}

/// Narrow a fresh scan to the decisions an operator reviewed.
///
/// A confirmation authorizes the reviewed reclaimable paths, and nothing else.
/// The fresh scan still decides whether each of them may go *now*: a reviewed
/// path is removed only if it is also reclaimable today, so a session that
/// became active, a directory Git now tracks, or a worktree no session records
/// any more keeps it. What changed elsewhere in the root -- another session's
/// build creating a `target/`, a kept candidate appearing or vanishing -- no
/// longer voids the review, which on a machine with many concurrent agents
/// made a reviewed plan nearly impossible to apply.
pub fn restrict_to_review(
    current: &[ReclaimCandidate],
    reviewed: &[ReclaimDecision],
) -> ReviewedScope {
    let authorized = reviewed
        .iter()
        .filter(|decision| decision.reclaimable)
        .map(|decision| decision.path.clone())
        .collect::<BTreeSet<_>>();
    let mut not_reviewed = Vec::new();
    let candidates = current
        .iter()
        .cloned()
        .map(|mut candidate| {
            if candidate.reclaimable && !authorized.contains(&candidate.path) {
                candidate.reclaimable = false;
                candidate.reason = "not in the reviewed plan".into();
                not_reviewed.push(candidate.path.clone());
            }
            candidate
        })
        .collect::<Vec<_>>();
    let withdrawn = authorized
        .iter()
        .filter_map(
            |path| match candidates.iter().find(|candidate| &candidate.path == path) {
                None => Some(format!("{}: no longer present", path.display())),
                Some(candidate) if !candidate.reclaimable => Some(format!(
                    "{}: now kept ({})",
                    path.display(),
                    candidate.reason
                )),
                Some(_) => None,
            },
        )
        .collect();
    not_reviewed.sort();
    ReviewedScope {
        candidates,
        withdrawn,
        not_reviewed,
    }
}

/// The worktree directory directly under `root` that is `worktree`, spelled
/// as a scan of `root` spells it, so a one-worktree plan names exactly the
/// paths the whole-root plan would.
///
/// `None` when `worktree` is not a direct child of `root`: such a worktree is
/// outside what this command may delete from, as it is for a whole-root scan.
pub fn worktree_in_root(root: &Path, worktree: &Path) -> Option<PathBuf> {
    let base = root.join(worktree.file_name()?);
    if !base.is_dir() {
        return None;
    }
    // Session rows and a scan can spell one directory differently, e.g.
    // macOS `/var/...` versus `/private/var/...`.
    let same = base == worktree
        || std::fs::canonicalize(&base)
            .is_ok_and(|found| std::fs::canonicalize(worktree).is_ok_and(|wanted| found == wanted));
    same.then_some(base)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ReclaimPlanSnapshot {
    schema_version: u8,
    digest: String,
    root: PathBuf,
    /// The one worktree a session-scoped review covered; absent for the
    /// whole root, which is how every earlier snapshot reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<PathBuf>,
    decisions: Vec<ReclaimDecision>,
}

/// A saved review, verified against the digest an operator confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewedPlan {
    pub digest: String,
    /// The one worktree the review covered, or `None` for the whole root.
    pub scope: Option<PathBuf>,
    pub decisions: Vec<ReclaimDecision>,
}

fn snapshot_path(root: &Path, digest: &str) -> io::Result<PathBuf> {
    if !is_reclaim_plan_digest(digest) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reclaim plan digest must be a 64-character hexadecimal SHA-256",
        ));
    }
    Ok(root.join(format!(
        "{RECLAIM_PLAN_FILENAME_PREFIX}{digest}{RECLAIM_PLAN_FILENAME_SUFFIX}"
    )))
}

/// Whether a reclaim confirmation has the exact digest syntax used by plan
/// snapshots. Keep this pure so CLI refusals can happen before opening broker
/// state or scanning a worktree root.
pub(crate) fn is_reclaim_plan_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn ensure_regular_snapshot(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(io::Error::other(format!(
            "reclaim plan snapshot is not a regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Save a reviewed decision set beside the host-scoped worktree root.
///
/// The digest is part of the filename so concurrent operators do not overwrite
/// one another's review evidence. An apply whose fresh scan no longer hashes to
/// the confirmed digest loads this snapshot to learn which paths were reviewed
/// reclaimable (see [`restrict_to_review`]).
pub fn save_snapshot(
    root: &Path,
    scope: Option<&Path>,
    digest: &str,
    candidates: &[ReclaimCandidate],
) -> io::Result<()> {
    std::fs::create_dir_all(root)?;
    let decisions = decisions(candidates);
    if decision_digest(root, scope, &decisions) != digest {
        return Err(io::Error::other(
            "reclaim plan snapshot digest does not match its decision set",
        ));
    }
    let path = snapshot_path(root, digest)?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => ensure_regular_snapshot(&path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let snapshot = ReclaimPlanSnapshot {
        schema_version: RECLAIM_PLAN_SCHEMA_VERSION,
        digest: digest.to_string(),
        root: root.to_path_buf(),
        scope: scope.map(Path::to_path_buf),
        decisions,
    };
    let bytes = serde_json::to_vec_pretty(&snapshot).map_err(io::Error::other)?;
    crate::atomic_file::with_synced_temporary(&path, &bytes, |temporary| {
        std::fs::rename(temporary, &path)
    })
}

/// Load and verify the saved decision set for a confirmation the fresh scan no
/// longer matches. An invalid or tampered snapshot authorizes nothing.
///
/// Verification binds the file to the confirmed digest: the decisions must
/// hash to it under this root. A forged snapshot therefore needs a different
/// digest, which the operator did not confirm. Even an authentic one only
/// narrows: every path it names is re-proved reclaimable by a fresh scan.
/// The scope is hashed too, so the returned scope is the one reviewed.
pub fn load_snapshot(root: &Path, digest: &str) -> io::Result<Option<ReviewedPlan>> {
    let path = snapshot_path(root, digest)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(io::Error::other(format!(
            "reclaim plan snapshot is not a regular file: {}",
            path.display()
        )));
    }
    let bytes = std::fs::read(&path)?;
    let snapshot: ReclaimPlanSnapshot = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if snapshot.schema_version != RECLAIM_PLAN_SCHEMA_VERSION
        || snapshot.root != root
        || snapshot.decisions != decisions_sorted(&snapshot.decisions)
        || decision_digest(root, snapshot.scope.as_deref(), &snapshot.decisions) != snapshot.digest
    {
        return Err(io::Error::other(format!(
            "reclaim plan snapshot is invalid: {}",
            path.display()
        )));
    }
    Ok(Some(ReviewedPlan {
        digest: snapshot.digest,
        scope: snapshot.scope,
        decisions: snapshot.decisions,
    }))
}

fn decisions_sorted(decisions: &[ReclaimDecision]) -> Vec<ReclaimDecision> {
    let mut sorted = decisions.to_vec();
    sorted.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.reclaimable.cmp(&right.reclaimable))
    });
    sorted
}

/// One reclaimable directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReclaimCandidate {
    pub path: PathBuf,
    /// Measured size; `None` (JSON `null`) when it was not measured -- an
    /// apply never measures, and a whole-root plan stops measuring when its
    /// sizing budget runs out. Never authorization-bearing.
    pub bytes: Option<u64>,
    /// The session worktree this belongs to.
    pub worktree: PathBuf,
    /// False when the session still needs it -- reported, never reclaimed.
    pub reclaimable: bool,
    pub reason: String,
}

/// Whether a directory name is regenerable build output in the shared
/// catalog (see [`crate::artifact_catalog`]).
pub fn is_artefact_directory(name: &str) -> bool {
    crate::artifact_catalog::is_catalogued(name, &[])
}

/// Whether a directory name is regenerable build output under the built-in
/// catalog plus additive configured names. Configuration never removes a
/// built-in name from the catalog.
pub fn is_artefact_directory_with_extras(name: &str, extras: &[String]) -> bool {
    crate::artifact_catalog::is_catalogued(name, extras)
}

/// Decide whether a candidate found under `worktree` may be reclaimed.
///
/// `active` names worktrees whose sessions are *working*, not merely open.
/// Their artefacts are reported so the space is accounted for, but never
/// removed: deleting a `target/` under a running build turns a disk problem
/// into a corrupt one.
///
/// The distinction is the whole point. Protecting every open session would
/// protect exactly the worktrees this exists to reclaim -- a session that
/// cannot be closed (#152) stays open indefinitely while doing nothing, and
/// its build output is the largest reclaimable thing on the disk. An idle or
/// stale session is named in the plan an operator reviews, so nothing is
/// deleted without someone seeing whose it was.
pub fn classify(path: &Path, worktree: &Path, bytes: u64, active: &[PathBuf]) -> ReclaimCandidate {
    // A session row and a scan can spell one directory differently: `start
    // --adopt` records the canonical `/private/var/...` where the scan walks
    // `/var/...` on macOS. Exact comparison alone let an adopted, working
    // session's build output be proposed for deletion.
    let is_active = active.iter().any(|candidate| candidate == worktree)
        || std::fs::canonicalize(worktree).is_ok_and(|worktree| {
            active
                .iter()
                .any(|candidate| std::fs::canonicalize(candidate).is_ok_and(|c| c == worktree))
        });
    ReclaimCandidate {
        path: path.to_path_buf(),
        worktree: worktree.to_path_buf(),
        bytes: Some(bytes),
        reclaimable: !is_active,
        reason: if is_active {
            "session is still active; a build may be running against it".into()
        } else {
            "regenerable build output for an inactive session".into()
        },
    }
}

/// Total measured bytes the reclaimable candidates would free. Unmeasured
/// candidates contribute nothing, so with any of them this is a lower bound.
pub fn reclaimable_bytes(candidates: &[ReclaimCandidate]) -> u64 {
    candidates
        .iter()
        .filter(|candidate| candidate.reclaimable)
        .filter_map(|candidate| candidate.bytes)
        .sum()
}

/// Total measured bytes of every candidate; a lower bound when any candidate
/// is unmeasured.
pub fn measured_bytes(candidates: &[ReclaimCandidate]) -> u64 {
    candidates
        .iter()
        .filter_map(|candidate| candidate.bytes)
        .sum()
}

/// Refuse to delete anything that is not inside `root`.
///
/// The plan is reviewed as text and applied later, so the path it names must be
/// re-proved to be somewhere this command is allowed to delete from. A `..`
/// component or an absolute path pointing elsewhere must not survive review.
pub fn is_within(root: &Path, path: &Path) -> bool {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return false;
    }
    path.starts_with(root) && path != root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(value: &str) -> PathBuf {
        PathBuf::from(value)
    }

    #[test]
    fn reclaim_plan_digest_syntax_is_shared_with_snapshot_paths() {
        assert!(is_reclaim_plan_digest(&"a".repeat(64)));
        assert!(is_reclaim_plan_digest(&"F".repeat(64)));
        assert!(!is_reclaim_plan_digest(&"a".repeat(63)));
        assert!(!is_reclaim_plan_digest(&format!("{}x", "a".repeat(63))));
    }

    #[test]
    fn known_build_directories_are_recognised() {
        for name in [
            "target",
            "node_modules",
            ".venv",
            ".pnpm-store",
            "build",
            "dist",
        ] {
            assert!(is_artefact_directory(name), "{name}");
        }
    }

    /// A heuristic like "large and gitignored" would also match a downloaded
    /// dataset or a local database. This command deletes; it stays narrow.
    #[test]
    fn other_ignored_directories_are_not_artefacts() {
        for name in ["data", "fixtures", ".aethyme", "logs", "coverage", ".git"] {
            assert!(!is_artefact_directory(name), "{name}");
        }
    }

    #[test]
    fn an_inactive_sessions_artefacts_are_reclaimable() {
        let candidate = classify(&p("/w/done/target"), &p("/w/done"), 7 << 30, &[]);
        assert!(candidate.reclaimable);
        assert_eq!(candidate.bytes, Some(7 << 30));
    }

    /// Deleting a `target/` under a running build turns a disk problem into a
    /// corrupt one.
    #[test]
    fn an_active_sessions_artefacts_are_reported_but_never_reclaimed() {
        let candidate = classify(
            &p("/w/live/target"),
            &p("/w/live"),
            9 << 30,
            &[p("/w/live")],
        );
        assert!(!candidate.reclaimable);
        assert!(
            candidate.reason.contains("still active"),
            "{}",
            candidate.reason
        );
    }

    /// `start --adopt` records the canonical spelling of a worktree the scan
    /// reaches through a symlink (`/var` versus `/private/var` on macOS).
    #[test]
    fn an_active_session_recorded_under_another_spelling_is_still_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("s/target")).unwrap();
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        let candidate = classify(
            &alias.join("s/target"),
            &alias.join("s"),
            8,
            &[real.join("s")],
        );
        assert!(!candidate.reclaimable, "{candidate:?}");
    }

    #[test]
    fn only_reclaimable_candidates_count_toward_the_total() {
        let candidates = vec![
            classify(&p("/w/a/target"), &p("/w/a"), 100, &[]),
            classify(&p("/w/b/target"), &p("/w/b"), 200, &[p("/w/b")]),
        ];
        assert_eq!(reclaimable_bytes(&candidates), 100);
    }

    #[test]
    fn plan_digest_ignores_measured_bytes_and_order() {
        let first = vec![
            classify(&p("/w/b/target"), &p("/w/b"), 200, &[p("/w/b")]),
            classify(&p("/w/a/target"), &p("/w/a"), 100, &[]),
        ];
        let second = vec![
            classify(&p("/w/a/target"), &p("/w/a"), 900, &[]),
            classify(&p("/w/b/target"), &p("/w/b"), 7, &[p("/w/b")]),
        ];
        assert_eq!(
            plan_digest(&p("/w"), None, &first),
            plan_digest(&p("/w"), None, &second)
        );
    }

    #[test]
    fn plan_digest_changes_for_candidate_set_or_reclaimability_changes() {
        let original = vec![classify(&p("/w/a/target"), &p("/w/a"), 100, &[])];
        let added = vec![
            classify(&p("/w/a/target"), &p("/w/a"), 100, &[]),
            classify(&p("/w/b/target"), &p("/w/b"), 100, &[]),
        ];
        let kept = vec![classify(&p("/w/a/target"), &p("/w/a"), 100, &[p("/w/a")])];
        assert_ne!(
            plan_digest(&p("/w"), None, &original),
            plan_digest(&p("/w"), None, &added)
        );
        assert_ne!(
            plan_digest(&p("/w"), None, &original),
            plan_digest(&p("/w"), None, &kept)
        );
    }

    fn reviewed(path: &str, reclaimable: bool) -> ReclaimDecision {
        ReclaimDecision {
            path: p(path),
            reclaimable,
        }
    }

    /// Another session's build creating a kept `target/` after review used to
    /// void every reviewed deletion.
    #[test]
    fn an_unrelated_change_does_not_withdraw_a_reviewed_deletion() {
        let current = vec![
            classify(&p("/w/done/target"), &p("/w/done"), 8, &[]),
            classify(&p("/w/live/target"), &p("/w/live"), 8, &[p("/w/live")]),
        ];
        let scope = restrict_to_review(&current, &[reviewed("/w/done/target", true)]);
        assert_eq!(reclaimable_bytes(&scope.candidates), 8);
        assert!(scope.candidates[0].reclaimable);
        assert!(scope.withdrawn.is_empty(), "{:?}", scope.withdrawn);
        assert!(scope.not_reviewed.is_empty());
    }

    /// The review authorizes; the fresh scan still decides whether now is safe.
    #[test]
    fn a_reviewed_path_the_fresh_scan_keeps_is_withdrawn() {
        let current = vec![classify(&p("/w/s/target"), &p("/w/s"), 8, &[p("/w/s")])];
        let scope = restrict_to_review(&current, &[reviewed("/w/s/target", true)]);
        assert_eq!(reclaimable_bytes(&scope.candidates), 0);
        assert_eq!(scope.withdrawn.len(), 1);
        assert!(
            scope.withdrawn[0].contains("now kept"),
            "{:?}",
            scope.withdrawn
        );
    }

    #[test]
    fn a_reviewed_path_that_vanished_is_reported_not_retried() {
        let scope = restrict_to_review(&[], &[reviewed("/w/s/target", true)]);
        assert!(scope.candidates.is_empty());
        assert_eq!(scope.withdrawn, vec!["/w/s/target: no longer present"]);
    }

    /// Nobody reviewed a path that appeared afterwards, however reclaimable.
    #[test]
    fn a_path_the_review_did_not_mark_reclaimable_is_never_authorized() {
        let current = vec![
            classify(&p("/w/new/target"), &p("/w/new"), 8, &[]),
            classify(&p("/w/was-kept/target"), &p("/w/was-kept"), 8, &[]),
        ];
        let scope = restrict_to_review(&current, &[reviewed("/w/was-kept/target", false)]);
        assert_eq!(reclaimable_bytes(&scope.candidates), 0);
        assert_eq!(
            scope.not_reviewed,
            vec![p("/w/new/target"), p("/w/was-kept/target")]
        );
        assert!(scope.withdrawn.is_empty());
    }

    /// A one-session review is not a whole-root review of the same
    /// decisions, and neither is it another session's.
    #[test]
    fn the_scope_is_part_of_what_a_digest_authorizes() {
        let candidates = vec![classify(&p("/w/a/target"), &p("/w/a"), 8, &[])];
        let whole = plan_digest(&p("/w"), None, &candidates);
        let scoped = plan_digest(&p("/w"), Some(&p("/w/a")), &candidates);
        assert_ne!(whole, scoped);
        assert_ne!(scoped, plan_digest(&p("/w"), Some(&p("/w/b")), &candidates));
    }

    #[test]
    fn a_scoped_snapshot_round_trips_its_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let worktree = root.join("s");
        let candidates = vec![classify(&worktree.join("target"), &worktree, 42, &[])];
        let digest = plan_digest(&root, Some(&worktree), &candidates);

        save_snapshot(&root, Some(&worktree), &digest, &candidates).unwrap();

        let reviewed = load_snapshot(&root, &digest).unwrap().unwrap();
        assert_eq!(reviewed.scope, Some(worktree));
        assert!(
            save_snapshot(&root, None, &digest, &candidates).is_err(),
            "a scoped digest must not be saved as a whole-root review"
        );
    }

    #[test]
    fn a_worktree_is_resolved_only_as_a_direct_child_of_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join("s/nested")).unwrap();
        assert_eq!(
            worktree_in_root(&root, &root.join("s")),
            Some(root.join("s"))
        );
        assert_eq!(worktree_in_root(&root, &root.join("s/nested")), None);
        assert_eq!(worktree_in_root(&root, &root.join("gone")), None);
        assert_eq!(worktree_in_root(&root, &tmp.path().join("s")), None);
    }

    #[test]
    fn a_saved_snapshot_round_trips_the_reviewed_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let candidates = vec![classify(&root.join("s/target"), &root.join("s"), 42, &[])];
        let digest = plan_digest(&root, None, &candidates);

        save_snapshot(&root, None, &digest, &candidates).unwrap();

        assert_eq!(
            load_snapshot(&root, &digest).unwrap(),
            Some(ReviewedPlan {
                digest,
                scope: None,
                decisions: vec![ReclaimDecision {
                    path: root.join("s/target"),
                    reclaimable: true,
                }],
            })
        );
    }

    #[test]
    fn snapshots_for_concurrent_reviews_are_keyed_by_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let first = classify(&root.join("first/target"), &root.join("first"), 42, &[]);
        let second = classify(&root.join("second/target"), &root.join("second"), 84, &[]);
        let first_digest = plan_digest(&root, None, std::slice::from_ref(&first));
        let second_digest = plan_digest(&root, None, std::slice::from_ref(&second));

        save_snapshot(&root, None, &first_digest, &[first]).unwrap();
        save_snapshot(&root, None, &second_digest, &[second]).unwrap();

        assert!(load_snapshot(&root, &first_digest).unwrap().is_some());
        assert!(load_snapshot(&root, &second_digest).unwrap().is_some());
        assert_eq!(
            std::fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .count(),
            2
        );
    }

    #[test]
    fn a_path_inside_the_root_is_allowed() {
        assert!(is_within(&p("/w"), &p("/w/session/target")));
    }

    /// The plan is reviewed as text and applied later, so a path that escapes
    /// the root must not survive that gap.
    #[test]
    fn escaping_paths_are_refused() {
        assert!(!is_within(&p("/w"), &p("/w/../etc")));
        assert!(!is_within(&p("/w"), &p("/etc/passwd")));
        assert!(
            !is_within(&p("/w"), &p("/w")),
            "the root itself is not a candidate"
        );
    }
}

/// A reviewed reclamation plan.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReclaimPlan {
    pub digest: String,
    pub root: PathBuf,
    /// The one worktree a session-scoped plan covers; absent for the root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<PathBuf>,
    pub candidates: Vec<ReclaimCandidate>,
    pub reclaimable_bytes: u64,
    pub total_bytes: u64,
}

/// What an apply actually removed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReclaimOutcome {
    pub removed: Vec<PathBuf>,
    pub reclaimed_bytes: u64,
    pub skipped: Vec<String>,
    /// Reclaimable now, but not in the reviewed plan, so left in place. A
    /// later plan includes them.
    pub not_reviewed: Vec<PathBuf>,
}

/// Directory size, following no symlinks.
///
/// A symlink into someone else's tree must contribute nothing to a total that
/// is about to justify a deletion.
pub fn directory_bytes(path: &Path) -> u64 {
    directory_bytes_within(path, &mut || false).unwrap_or(0)
}

/// [`directory_bytes`], abandoned as soon as `expired` says the budget is
/// spent. `None` means "not measured", never "empty": a partial count of a
/// tree is not its size, so it is discarded rather than reported.
///
/// `expired` is consulted before each directory and each entry, so a single
/// enormous `node_modules` cannot hold a plan past its budget. Tests pass a
/// counting closure instead of a clock.
pub fn directory_bytes_within(path: &Path, expired: &mut dyn FnMut() -> bool) -> Option<u64> {
    if expired() {
        return None;
    }
    let mut total: u64 = 0;
    let Ok(entries) = read_dir(path) else {
        return Some(0);
    };
    for entry in entries.flatten() {
        if expired() {
            return None;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            total = total.saturating_add(directory_bytes_within(&entry.path(), expired)?);
        } else if let Ok(meta) = entry.metadata() {
            total = total.saturating_add(meta.len());
        }
    }
    Some(total)
}

/// Scan `root` for reclaimable artefacts, one level inside each worktree.
///
/// Scoped to `<root>/<worktree>/**/<artefact-dir>` and stops descending once it
/// finds one -- a `target/` inside a `target/` is already accounted for, and
/// walking into a multi-gigabyte build tree to confirm that is pure cost.
pub fn scan(root: &Path, active: &[PathBuf]) -> Vec<ReclaimCandidate> {
    scan_with_extra_directories(root, active, &[])
}

/// Scan `root` with additive configured artifact directory names.
pub fn scan_with_extra_directories(
    root: &Path,
    active: &[PathBuf],
    extras: &[String],
) -> Vec<ReclaimCandidate> {
    scan_root(root, active, extras, Sizing::Measure)
}

/// Whether a scan measures each candidate's size.
///
/// Measuring walks every file under every candidate -- a `node_modules` or a
/// `target/` holds hundreds of thousands -- and dominates a scan. A plan
/// shows the sizes an operator reviews; an apply never reports them (it
/// credits what the removal actually freed), so it skips the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sizing {
    Measure,
    Skip,
}

/// Scan every worktree directly under `root`.
pub fn scan_root(
    root: &Path,
    active: &[PathBuf],
    extras: &[String],
    sizing: Sizing,
) -> Vec<ReclaimCandidate> {
    scan_root_within(root, active, extras, sizing, &mut || false).0
}

/// How far a whole-root candidate discovery got.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Discovery {
    /// Every worktree under the root was walked, so the candidate list is
    /// the whole root's. A plan with `false` here cannot be applied.
    pub complete: bool,
    /// The discovery budget, or `None` when it was unbounded.
    pub budget_ms: Option<u64>,
    pub worktrees_scanned: usize,
    pub worktrees_not_scanned: usize,
    pub elapsed_ms: u64,
}

/// [`scan_root`] that stops starting new worktrees once `expired` says the
/// discovery budget is spent, and says so.
///
/// The budget is checked between worktrees, so each listed worktree's
/// candidates are complete; the walk inside one worktree is already bounded
/// by its depth limit. Worktrees are walked in name order so a cut-off plan
/// is reproducible.
pub fn scan_root_within(
    root: &Path,
    active: &[PathBuf],
    extras: &[String],
    sizing: Sizing,
    expired: &mut dyn FnMut() -> bool,
) -> (Vec<ReclaimCandidate>, Discovery) {
    let mut found = Vec::new();
    let mut discovery = Discovery {
        complete: true,
        ..Discovery::default()
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return (found, discovery);
    };
    let mut worktrees: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    worktrees.sort();
    for worktree in &worktrees {
        if expired() {
            discovery.complete = false;
            discovery.worktrees_not_scanned = worktrees.len() - discovery.worktrees_scanned;
            break;
        }
        found.extend(scan_worktree(worktree, active, extras, sizing));
        discovery.worktrees_scanned += 1;
    }
    found.sort_by_key(|candidate| std::cmp::Reverse(candidate.bytes));
    (found, discovery)
}

/// Scan one worktree, and nothing else under its root.
///
/// A plan for one session used to scan and size the whole root and then
/// filter: with dozens of worktrees full of `node_modules`, one session's
/// plan took tens of minutes, and its apply paid the same again.
pub fn scan_worktree(
    worktree: &Path,
    active: &[PathBuf],
    extras: &[String],
    sizing: Sizing,
) -> Vec<ReclaimCandidate> {
    let mut found = Vec::new();
    let checkout = own_checkout(worktree);
    let walk = Walk {
        worktree,
        checkout: checkout.as_ref(),
        active,
        extras,
        sizing,
    };
    walk.collect(worktree, &mut found, 0);
    found.sort_by_key(|candidate| std::cmp::Reverse(candidate.bytes));
    found
}

/// The Git checkout rooted exactly at `worktree`, if there is one.
///
/// Discovery walks upward, so a directory that is not itself a checkout could
/// resolve to some enclosing repository whose ignore rules say nothing about it.
fn own_checkout(worktree: &Path) -> Option<crate::git::GitRepo> {
    let checkout = crate::git::GitRepo::discover(worktree).ok()?;
    let same = |path: &Path| std::fs::canonicalize(path).ok();
    (same(checkout.root()) == same(worktree)).then_some(checkout)
}

/// Why Git forbids reclaiming `path`, if it does.
///
/// A matching name is not evidence of build output: projects commit `dist/`
/// and `build/`, and deleting those destroyed tracked files. Only a
/// directory Git ignores and holds no tracked file under is disposable.
fn git_protection(
    checkout: Option<&crate::git::GitRepo>,
    worktree: &Path,
    path: &Path,
) -> Option<String> {
    let Some(checkout) = checkout else {
        return Some("not a Git checkout, so nothing shows this directory is disposable".into());
    };
    let Some(relative) = path
        .strip_prefix(worktree)
        .ok()
        .and_then(|relative| relative.to_str())
    else {
        return Some("path cannot be expressed relative to its worktree".into());
    };
    // Tracked files first: `check-ignore` also reports a directory holding
    // them as not ignored, and "tracked" is the reason that matters.
    match checkout.is_tracked(relative) {
        Ok(false) => {}
        Ok(true) => return Some("Git tracks files under this directory".into()),
        Err(error) => return Some(format!("cannot check for tracked files: {error}")),
    }
    (!checkout.path_is_ignored(relative))
        .then(|| "Git does not ignore this directory, so it is not known to be build output".into())
}

/// Keep candidates in worktrees no session of this repository records.
///
/// "Not active" must not include "unknown": a worktree whose session row was
/// lost, or that was made outside the broker, has no owner who reviewed it.
pub fn protect_unrecorded_worktrees(candidates: &mut [ReclaimCandidate], recorded: &[PathBuf]) {
    // Session rows and a scan can spell one directory differently, e.g.
    // macOS `/var/...` versus `/private/var/...`.
    let canonical: Vec<PathBuf> = recorded
        .iter()
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .collect();
    for candidate in candidates.iter_mut().filter(|c| c.reclaimable) {
        let known = recorded.contains(&candidate.worktree)
            || std::fs::canonicalize(&candidate.worktree)
                .is_ok_and(|worktree| canonical.contains(&worktree));
        if !known {
            candidate.reclaimable = false;
            candidate.reason = "no session of this repository records this worktree".into();
        }
    }
}

/// How long after a worktree's Git index last changed its build output is
/// still treated as in use.
///
/// `Active` is ten minutes of hook activity, which an agent thinking through a
/// long change, or a person working in the checkout without the hook, falls
/// outside of. A Git index changes on every `add`, `commit`, `checkout` and
/// index-refreshing `status`, so it is a cheap witness that someone worked
/// here recently whatever the session row says.
pub const RECENT_WORK_WINDOW_MS: i64 = 3 * 3_600_000;

/// Process attribution for one named file from a complete `lsof` snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenFileUse {
    pub pid: u32,
    pub command: String,
    pub descriptor: String,
    pub file_type: Option<String>,
    pub path: PathBuf,
}

/// A process-use scan is either complete or explicitly unknown. A partial
/// `lsof` listing must never be treated as an empty listing by a caller that
/// decides whether to delete build output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpenFileSnapshot {
    Complete(Vec<OpenFileUse>),
    Incomplete {
        reason: String,
        observed: Vec<OpenFileUse>,
    },
}

/// Keep a worktree candidate when a complete snapshot attributes an open path
/// to a process. The reason retains enough identity to find the process and
/// file during a cleanup review.
pub(crate) fn protect_worktrees_with_open_file_uses(
    candidates: &mut [ReclaimCandidate],
    open_files: &[OpenFileUse],
) {
    for candidate in candidates
        .iter_mut()
        .filter(|candidate| candidate.reclaimable)
    {
        if let Some(open) = open_file_use_for_worktree(&candidate.worktree, open_files) {
            candidate.reclaimable = false;
            candidate.reason = open_file_use_reason(open);
        }
    }
}

/// Keep all otherwise-reclaimable candidates when process use could not be
/// checked. The failure reason is deliberately bounded and is not evidence
/// that a worktree is idle.
pub(crate) fn protect_worktrees_when_open_file_snapshot_is_incomplete(
    candidates: &mut [ReclaimCandidate],
    reason: &str,
    observed: &[OpenFileUse],
) {
    for candidate in candidates
        .iter_mut()
        .filter(|candidate| candidate.reclaimable)
    {
        candidate.reclaimable = false;
        candidate.reason = incomplete_open_file_reason(
            reason,
            open_file_use_for_worktree(&candidate.worktree, observed),
        );
    }
}

pub(crate) fn open_file_snapshot_blocker_for_worktree(
    snapshot: &OpenFileSnapshot,
    worktree: &Path,
) -> Option<String> {
    match snapshot {
        OpenFileSnapshot::Complete(open_files) => {
            open_file_use_for_worktree(worktree, open_files).map(open_file_use_reason)
        }
        OpenFileSnapshot::Incomplete { reason, observed } => Some(incomplete_open_file_reason(
            reason,
            open_file_use_for_worktree(worktree, observed),
        )),
    }
}

/// Report process use of one session worktree. For a live task holder only,
/// its exact `cwd` directory record is harmless to metadata retirement: the
/// checkout itself remains protected from physical removal by the normal
/// cleanup checks. Every other descriptor, and every incomplete inventory,
/// blocks retirement.
pub(crate) fn open_file_snapshot_blocker_for_session_worktree(
    snapshot: &OpenFileSnapshot,
    worktree: &Path,
    live_holder_pid: Option<i64>,
) -> Option<String> {
    let canonical = std::fs::canonicalize(worktree).ok();
    let is_under_worktree = |open: &OpenFileUse| {
        open.path.starts_with(worktree)
            || canonical
                .as_ref()
                .is_some_and(|canonical| open.path.starts_with(canonical))
    };
    let is_holder_cwd = |open: &OpenFileUse| {
        live_holder_pid == Some(i64::from(open.pid))
            && open.descriptor == "cwd"
            && open.file_type.as_deref() == Some("DIR")
            && canonical
                .as_ref()
                .is_some_and(|canonical| open.path == *canonical)
    };
    match snapshot {
        OpenFileSnapshot::Complete(open_files) => open_files
            .iter()
            .find(|open| is_under_worktree(open) && !is_holder_cwd(open))
            .map(open_file_use_reason),
        OpenFileSnapshot::Incomplete { reason, observed } => Some(incomplete_open_file_reason(
            reason,
            observed
                .iter()
                .find(|open| is_under_worktree(open) && !is_holder_cwd(open)),
        )),
    }
}

fn open_file_use_for_worktree<'a>(
    worktree: &Path,
    open_files: &'a [OpenFileUse],
) -> Option<&'a OpenFileUse> {
    let canonical = std::fs::canonicalize(worktree).ok();
    open_files.iter().find(|open| {
        open.path.starts_with(worktree)
            || canonical
                .as_ref()
                .is_some_and(|canonical| open.path.starts_with(canonical))
    })
}

fn incomplete_open_file_reason(reason: &str, observed: Option<&OpenFileUse>) -> String {
    let reason = bounded_reason(reason, 240);
    if let Some(open) = observed {
        bounded_reason(
            &format!(
                "{}; lsof snapshot incomplete ({reason}); kept for safety",
                open_file_use_reason(open)
            ),
            520,
        )
    } else {
        format!("process use could not be checked ({reason}); kept for safety")
    }
}

pub(crate) fn open_file_use_reason(open: &OpenFileUse) -> String {
    format!(
        "process {} ({}) has {} open",
        open.pid,
        bounded_process_command(&open.command),
        bounded_open_path(&open.path)
    )
}

fn bounded_process_command(command: &str) -> String {
    bounded_reason(&format!("{command:?}"), 120)
}

fn bounded_open_path(path: &Path) -> String {
    bounded_reason(&format!("{path:?}"), 360)
}

fn bounded_reason(reason: &str, max_bytes: usize) -> String {
    let mut bounded = String::new();
    for character in reason.chars() {
        if bounded.len() + character.len_utf8() + '…'.len_utf8() > max_bytes {
            bounded.push('…');
            break;
        }
        bounded.push(character);
    }
    bounded
}

/// Keep candidates in worktrees whose Git index changed within `window_ms`.
///
/// An index whose timestamp cannot be read counts as recent: not knowing when
/// someone last worked here is not evidence that nobody did.
pub fn protect_recently_worked_worktrees(
    candidates: &mut [ReclaimCandidate],
    now_ms: i64,
    window_ms: i64,
) {
    for candidate in candidates.iter_mut().filter(|c| c.reclaimable) {
        let Some(index) = git_index_path(&candidate.worktree) else {
            continue;
        };
        if !index.exists() {
            continue;
        }
        let age_ms = std::fs::metadata(&index)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| now_ms.saturating_sub(since.as_millis() as i64));
        match age_ms {
            Some(age_ms) if age_ms >= window_ms => {}
            Some(age_ms) => {
                candidate.reclaimable = false;
                candidate.reason = format!(
                    "Git index changed {} min ago; someone worked here within the last {} h",
                    age_ms / 60_000,
                    window_ms / 3_600_000
                );
            }
            None => {
                candidate.reclaimable = false;
                candidate.reason = "cannot read when this worktree's Git index last changed".into();
            }
        }
    }
}

/// The index file of the checkout rooted at `worktree`: `.git/index` for a
/// primary checkout, `<gitdir>/index` for a linked worktree whose `.git` is a
/// `gitdir:` pointer file.
fn git_index_path(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    let metadata = std::fs::symlink_metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        return Some(dot_git.join("index"));
    }
    let pointer = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = pointer
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    Some(worktree.join(gitdir).join("index"))
}

/// Runtime and output bounds for a system-wide process-use observation.
const OPEN_FILE_SNAPSHOT_BUDGET: Duration = Duration::from_secs(10);
const OPEN_FILE_SNAPSHOT_MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

/// One bounded, system-wide `lsof` snapshot filtered to names beneath `root`.
///
/// `lsof +D` is intentionally avoided: it walks the complete cache tree. The
/// command is supervised by `bounded_output`, and timeout, output-limit,
/// nonzero-exit, malformed, or empty results remain `Incomplete` rather than
/// being interpreted as no process use.
pub(crate) fn open_file_snapshot_under(root: &Path) -> OpenFileSnapshot {
    open_file_snapshot_under_with_budget(root, OPEN_FILE_SNAPSHOT_BUDGET)
}

/// Make one bounded system-wide `lsof` observation within the caller's
/// remaining maintenance budget. A zero budget does not start the process.
pub(crate) fn open_file_snapshot_under_with_budget(
    root: &Path,
    budget: Duration,
) -> OpenFileSnapshot {
    let budget = budget.min(OPEN_FILE_SNAPSHOT_BUDGET);
    if budget.is_zero() {
        return incomplete_open_file_snapshot("maintenance deadline left no time for lsof");
    }
    // `lsof` lives in /usr/sbin on macOS, which a trimmed PATH (a launchd
    // job, a test harness) leaves out; the standard locations are tried too
    // (#588). A successfully spawned but incomplete listing is not retried as
    // another binary and never becomes a successful empty snapshot.
    for program in ["lsof", "/usr/sbin/lsof", "/usr/bin/lsof", "/sbin/lsof"] {
        let mut command = std::process::Command::new(program);
        command.args(["-w", "-n", "-P", "-F0pcfnt"]);
        match crate::bounded_output::output_limited(
            &mut command,
            budget,
            OPEN_FILE_SNAPSHOT_MAX_STREAM_BYTES,
        ) {
            Ok(output) => return snapshot_from_lsof_capture(output, root),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return incomplete_open_file_snapshot(format!(
                    "could not run lsof ({program}): {error}"
                ));
            }
        }
    }
    incomplete_open_file_snapshot("lsof was not found in PATH or standard locations")
}

fn snapshot_from_lsof_capture(
    output: crate::bounded_output::LimitedOutput,
    root: &Path,
) -> OpenFileSnapshot {
    match output {
        crate::bounded_output::LimitedOutput::Completed(output) => parse_lsof_output(
            output.status.success(),
            &output.stdout,
            &output.stderr,
            root,
        ),
        crate::bounded_output::LimitedOutput::Deadline => incomplete_open_file_snapshot(format!(
            "lsof exceeded its {} second time limit",
            OPEN_FILE_SNAPSHOT_BUDGET.as_secs()
        )),
        crate::bounded_output::LimitedOutput::OutputLimit => {
            incomplete_open_file_snapshot(format!(
                "lsof output exceeded its {} MiB limit",
                OPEN_FILE_SNAPSHOT_MAX_STREAM_BYTES / (1024 * 1024)
            ))
        }
    }
}

fn incomplete_open_file_snapshot(reason: impl Into<String>) -> OpenFileSnapshot {
    OpenFileSnapshot::Incomplete {
        reason: reason.into(),
        observed: Vec::new(),
    }
}

fn parse_lsof_output(
    successful_exit: bool,
    stdout: &[u8],
    stderr: &[u8],
    root: &Path,
) -> OpenFileSnapshot {
    let (observed, parse_error) = match parse_lsof_records(stdout, root) {
        Ok(open_files) => (open_files, None),
        Err((open_files, reason)) => (open_files, Some(reason)),
    };
    if !successful_exit {
        return OpenFileSnapshot::Incomplete {
            reason: "lsof exited unsuccessfully; its listing may be partial".into(),
            observed,
        };
    }
    if !stderr.is_empty() {
        return OpenFileSnapshot::Incomplete {
            reason: "lsof wrote diagnostics; completeness of its listing is unknown".into(),
            observed,
        };
    }
    match parse_error {
        None => OpenFileSnapshot::Complete(observed),
        Some(reason) => OpenFileSnapshot::Incomplete { reason, observed },
    }
}

/// Parse NUL-delimited `lsof -F0pcfnt` records. PID and command context is
/// required for every pathname. lsof may append one linefeed after the final
/// NUL field delimiter. Other malformed or truncated suffixes are rejected;
/// paths from preceding complete records remain useful positive evidence but
/// never make the snapshot complete.
fn parse_lsof_records(
    listing: &[u8],
    root: &Path,
) -> Result<Vec<OpenFileUse>, (Vec<OpenFileUse>, String)> {
    if listing.is_empty() {
        return Err((Vec::new(), "lsof returned an empty listing".into()));
    }

    let listing = if listing.ends_with(b"\0\n") {
        &listing[..listing.len() - 1]
    } else {
        listing
    };
    let terminated = listing.ends_with(&[0]);
    let fields_bytes = if terminated {
        &listing[..listing.len() - 1]
    } else {
        listing
    };
    let fields = fields_bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let complete_field_count = if terminated {
        fields.len()
    } else {
        fields.len().saturating_sub(1)
    };

    let canonical_root = std::fs::canonicalize(root).ok();
    let mut pid = None;
    let mut command = None;
    let mut saw_process = false;
    let mut saw_name = false;
    let mut saw_file_for_process = false;
    let mut awaiting_name = false;
    let mut descriptor: Option<String> = None;
    let mut file_type: Option<String> = None;
    let mut open_files = Vec::new();

    for field in fields.into_iter().take(complete_field_count) {
        let field = field.strip_prefix(b"\n").unwrap_or(field);
        let Some((&kind, value)) = field.split_first() else {
            return Err((open_files, "lsof listing contained an empty field".into()));
        };
        if value.is_empty() && kind != b'n' {
            return Err((
                open_files,
                "lsof listing contained an empty field value".into(),
            ));
        }
        match kind {
            b'p' => {
                if saw_process && command.is_none() {
                    return Err((open_files, "lsof process record had no command name".into()));
                }
                if awaiting_name || (saw_process && !saw_file_for_process) {
                    return Err((
                        open_files,
                        "lsof process record had an unnamed open-file record".into(),
                    ));
                }
                let Some(parsed_pid) = std::str::from_utf8(value)
                    .ok()
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|pid| *pid != 0)
                else {
                    return Err((open_files, "lsof process record had an invalid PID".into()));
                };
                pid = Some(parsed_pid);
                command = None;
                saw_process = true;
                saw_file_for_process = false;
                descriptor = None;
                file_type = None;
            }
            b'c' => {
                if pid.is_none() {
                    return Err((open_files, "lsof command field preceded its PID".into()));
                }
                let Some(parsed_command) = std::str::from_utf8(value)
                    .ok()
                    .filter(|command| !command.is_empty())
                else {
                    return Err((
                        open_files,
                        "lsof process record had an invalid command".into(),
                    ));
                };
                if command
                    .as_deref()
                    .is_some_and(|command| command != parsed_command)
                {
                    return Err((
                        open_files,
                        "lsof process record had conflicting command names".into(),
                    ));
                }
                command = Some(parsed_command.to_string());
            }
            b'f' => {
                if pid.is_none() || command.is_none() {
                    return Err((
                        open_files,
                        "lsof file descriptor preceded its process identity".into(),
                    ));
                }
                if awaiting_name {
                    return Err((
                        open_files,
                        "lsof file descriptor had no pathname record".into(),
                    ));
                }
                let Some(parsed_descriptor) = std::str::from_utf8(value)
                    .ok()
                    .filter(|descriptor| !descriptor.is_empty())
                else {
                    return Err((
                        open_files,
                        "lsof file descriptor had an invalid descriptor name".into(),
                    ));
                };
                awaiting_name = true;
                saw_file_for_process = true;
                descriptor = Some(parsed_descriptor.to_string());
                file_type = None;
            }
            b't' => {
                if pid.is_none() || command.is_none() || !awaiting_name {
                    return Err((
                        open_files,
                        "lsof file type preceded its file descriptor".into(),
                    ));
                }
                if file_type.is_some() {
                    return Err((
                        open_files,
                        "lsof file descriptor had conflicting file types".into(),
                    ));
                }
                let Some(parsed_type) = std::str::from_utf8(value)
                    .ok()
                    .filter(|file_type| !file_type.is_empty())
                else {
                    return Err((
                        open_files,
                        "lsof file descriptor had an invalid type".into(),
                    ));
                };
                file_type = Some(parsed_type.to_string());
            }
            b'n' => {
                let Some(pid) = pid else {
                    return Err((
                        open_files,
                        "lsof path field preceded its process record".into(),
                    ));
                };
                let Some(command) = command.as_ref() else {
                    return Err((open_files, "lsof path field had no process command".into()));
                };
                if !awaiting_name {
                    return Err((
                        open_files,
                        "lsof pathname had no preceding file descriptor".into(),
                    ));
                }
                saw_name = true;
                awaiting_name = false;
                if value.is_empty() {
                    // macOS reports these named descriptor kinds with an
                    // empty `n` field. They do not refer to filesystem paths;
                    // keep other empty or unknown names incomplete so a
                    // missing path cannot authorize deletion.
                    if matches!(file_type.as_deref(), Some("NPOLICY" | "NEXUS" | "PIPE")) {
                        descriptor = None;
                        file_type = None;
                        continue;
                    }
                    return Err((
                        open_files,
                        "lsof listing contained an empty field value".into(),
                    ));
                }
                let Some(path) = path_from_lsof_bytes(value) else {
                    return Err((
                        open_files,
                        "lsof path field could not be represented on this platform".into(),
                    ));
                };
                // Non-path names (for example socket descriptions) cannot
                // name an entry beneath a worktree. Filesystem paths are
                // absolute in lsof's name field.
                if path.is_absolute()
                    && (path.starts_with(root)
                        || canonical_root
                            .as_ref()
                            .is_some_and(|root| path.starts_with(root)))
                {
                    open_files.push(OpenFileUse {
                        pid,
                        command: command.clone(),
                        descriptor: descriptor
                            .as_deref()
                            .expect("an open pathname follows a descriptor")
                            .to_string(),
                        file_type: file_type.clone(),
                        path,
                    });
                }
                descriptor = None;
                file_type = None;
            }
            _ => {
                return Err((
                    open_files,
                    "lsof listing contained an unexpected field".into(),
                ));
            }
        }
    }
    if !terminated {
        return Err((open_files, "lsof listing was not NUL-terminated".into()));
    }
    if awaiting_name {
        return Err((
            open_files,
            "lsof file descriptor had no pathname record".into(),
        ));
    }
    if !saw_process || command.is_none() {
        return Err((
            open_files,
            "lsof listing contained no complete process record".into(),
        ));
    }
    if !saw_name {
        return Err((
            open_files,
            "lsof listing contained no pathname records".into(),
        ));
    }
    if !saw_file_for_process {
        return Err((
            open_files,
            "lsof process record had no file descriptors".into(),
        ));
    }
    Ok(open_files)
}

#[cfg(unix)]
fn path_from_lsof_bytes(bytes: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn path_from_lsof_bytes(bytes: &[u8]) -> Option<PathBuf> {
    String::from_utf8(bytes.to_vec()).ok().map(PathBuf::from)
}

/// What stays fixed while one worktree is walked.
struct Walk<'a> {
    worktree: &'a Path,
    checkout: Option<&'a crate::git::GitRepo>,
    active: &'a [PathBuf],
    extras: &'a [String],
    sizing: Sizing,
}

impl Walk<'_> {
    fn collect(&self, dir: &Path, found: &mut Vec<ReclaimCandidate>, depth: usize) {
        // Build trees are shallow relative to a repository; this bounds the
        // walk on a directory whose whole problem is that it is enormous.
        if depth > 6 {
            return;
        }
        let Ok(entries) = read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() || kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name == ".git" {
                continue;
            }
            if is_artefact_directory_with_extras(name, self.extras) {
                let mut candidate = classify(&path, self.worktree, 0, self.active);
                candidate.bytes = match self.sizing {
                    Sizing::Measure => Some(directory_bytes(&path)),
                    Sizing::Skip => None,
                };
                if candidate.reclaimable
                    && let Some(reason) = git_protection(self.checkout, self.worktree, &path)
                {
                    candidate.reclaimable = false;
                    candidate.reason = reason;
                }
                found.push(candidate);
                continue;
            }
            self.collect(&path, found, depth + 1);
        }
    }
}

/// `read_dir`, recording each directory a test scan opens.
fn read_dir(path: &Path) -> io::Result<std::fs::ReadDir> {
    #[cfg(test)]
    WALKED.with(|walked| walked.borrow_mut().push(path.to_path_buf()));
    std::fs::read_dir(path)
}

#[cfg(test)]
thread_local! {
    /// Directories this thread's scans opened, so a test can prove what a
    /// scan did not walk -- the filtered result alone cannot show that.
    static WALKED: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// How long a whole-root plan may spend finding candidates before it stops
/// starting new worktrees and reports itself partial. Measured on the busiest
/// repository (about 626 checkouts) an unbounded plan ran 8-16+ minutes.
pub const WHOLE_ROOT_DISCOVERY_BUDGET: Duration = Duration::from_secs(60);

/// How long a whole-root plan may spend measuring candidate sizes in total.
/// Sizes are what an operator reads, never what a confirmation authorizes, so
/// a plan that lists every candidate and sizes the largest share it can is
/// worth more than one that never returns.
pub const WHOLE_ROOT_SIZING_BUDGET: Duration = Duration::from_secs(20);

/// How old a measurement another whole-root plan saved may be and still be
/// shown instead of walking that directory again. Long enough to cover
/// several agents reacting to the same low-disk advice within a minute or
/// so; short enough that a build growing by gigabytes is not hidden for long.
/// Sizes never authorize anything, so a stale one costs accuracy, not safety.
pub const SIZE_REUSE_WINDOW_MS: i64 = 90_000;

/// A marker whose process start time cannot be read (an unsupported
/// platform) is trusted as live only for this long after it was written.
const IN_FLIGHT_UNVERIFIED_MAX_AGE_MS: i64 = 10 * 60_000;

/// A point in time a budget runs out at, or none for an unbounded run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline(Option<Instant>);

impl Deadline {
    /// `budget` from now; an unrepresentable instant is no deadline.
    pub fn after(budget: Duration) -> Self {
        Self(Instant::now().checked_add(budget))
    }

    pub fn never() -> Self {
        Self(None)
    }

    pub fn passed(&self) -> bool {
        self.0.is_some_and(|deadline| Instant::now() >= deadline)
    }
}

/// What a whole-root plan measured, reused, and left unmeasured.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SizingReport {
    /// The sizing budget, or `None` when `--full-scan` lifted it.
    pub budget_ms: Option<u64>,
    /// Candidates this plan walked and sized.
    pub measured: usize,
    /// Candidates sized from another whole-root plan's measurement at most
    /// [`SIZE_REUSE_WINDOW_MS`] old.
    pub reused: usize,
    /// Candidates listed with `bytes: null`.
    pub unmeasured: usize,
    /// The budget ran out before every candidate was sized.
    pub budget_exhausted: bool,
    /// Another whole-root plan was measuring this root, so this one did not
    /// walk the same directories at the same time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred_to: Option<InFlightPlan>,
    pub elapsed_ms: u64,
}

/// Size `candidates` in place, reclaimable ones first.
///
/// A candidate with a recent measurement in `recent` takes it. Otherwise it
/// is walked only when `walk` is true and the budget is not spent; a walk the
/// budget interrupts is discarded. Everything not sized is left `None` and
/// counted, never shown as zero. Returns the report and the fresh
/// measurements to share with the next plan.
pub fn measure_within(
    candidates: &mut [ReclaimCandidate],
    recent: &BTreeMap<PathBuf, u64>,
    walk: bool,
    expired: &mut dyn FnMut() -> bool,
) -> (SizingReport, Vec<(PathBuf, u64)>) {
    let mut report = SizingReport::default();
    let mut fresh = Vec::new();
    // Reclaimable first: they are what the totals an operator acts on count.
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by_key(|&index| !candidates[index].reclaimable);
    for index in order {
        let candidate = &mut candidates[index];
        if let Some(&bytes) = recent.get(&candidate.path) {
            candidate.bytes = Some(bytes);
            report.reused += 1;
            continue;
        }
        if !walk || report.budget_exhausted {
            candidate.bytes = None;
            report.unmeasured += 1;
            continue;
        }
        match directory_bytes_within(&candidate.path, expired) {
            Some(bytes) => {
                candidate.bytes = Some(bytes);
                report.measured += 1;
                fresh.push((candidate.path.clone(), bytes));
            }
            None => {
                candidate.bytes = None;
                report.unmeasured += 1;
                report.budget_exhausted = true;
            }
        }
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.bytes));
    (report, fresh)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SizeCache {
    schema_version: u8,
    root: PathBuf,
    entries: Vec<SizeCacheEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SizeCacheEntry {
    path: PathBuf,
    bytes: u64,
    measured_at_ms: i64,
}

fn size_cache_entries(root: &Path, now_ms: i64, window_ms: i64) -> Vec<SizeCacheEntry> {
    let path = root.join(RECLAIM_SIZE_CACHE_FILENAME);
    if ensure_regular_snapshot(&path).is_err() {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let Ok(cache) = serde_json::from_slice::<SizeCache>(&bytes) else {
        return Vec::new();
    };
    if cache.schema_version != RECLAIM_SIZE_CACHE_SCHEMA_VERSION || cache.root != root {
        return Vec::new();
    }
    cache
        .entries
        .into_iter()
        .filter(|entry| {
            entry.measured_at_ms <= now_ms
                && now_ms - entry.measured_at_ms <= window_ms
                && is_within(root, &entry.path)
        })
        .collect()
}

/// Sizes another whole-root plan of this root measured within `window_ms`.
/// Anything unreadable, foreign or old is simply not reused.
pub fn recent_sizes(root: &Path, now_ms: i64, window_ms: i64) -> BTreeMap<PathBuf, u64> {
    size_cache_entries(root, now_ms, window_ms)
        .into_iter()
        .map(|entry| (entry.path, entry.bytes))
        .collect()
}

/// Share fresh measurements with the next whole-root plan of this root.
/// Entries still inside the window are kept with their original time, so a
/// reused size never looks younger than it is.
pub fn record_sizes(
    root: &Path,
    now_ms: i64,
    window_ms: i64,
    measured: &[(PathBuf, u64)],
) -> io::Result<()> {
    if measured.is_empty() {
        return Ok(());
    }
    let path = root.join(RECLAIM_SIZE_CACHE_FILENAME);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => ensure_regular_snapshot(&path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut entries = BTreeMap::new();
    for entry in size_cache_entries(root, now_ms, window_ms) {
        entries.insert(entry.path.clone(), entry);
    }
    for (candidate, bytes) in measured {
        entries.insert(
            candidate.clone(),
            SizeCacheEntry {
                path: candidate.clone(),
                bytes: *bytes,
                measured_at_ms: now_ms,
            },
        );
    }
    let cache = SizeCache {
        schema_version: RECLAIM_SIZE_CACHE_SCHEMA_VERSION,
        root: root.to_path_buf(),
        entries: entries.into_values().collect(),
    };
    let bytes = serde_json::to_vec(&cache).map_err(io::Error::other)?;
    crate::atomic_file::with_synced_temporary(&path, &bytes, |temporary| {
        std::fs::rename(temporary, &path)
    })
}

/// The process running a whole-root plan, as its in-flight marker names it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InFlightPlan {
    pub pid: u32,
    /// The process start time, which tells a reused pid from the holder.
    #[serde(default)]
    pub process_start: Option<u64>,
    pub started_at_ms: i64,
}

impl InFlightPlan {
    /// This process.
    pub fn current(now_ms: i64) -> Self {
        let pid = std::process::id();
        Self {
            pid,
            process_start: i32::try_from(pid)
                .ok()
                .and_then(crate::gates::process_start_time),
            started_at_ms: now_ms,
        }
    }
}

/// Whether the process a marker names is still the one that wrote it: the
/// same pid with the same start time. Without a start time, a live pid is
/// trusted only for [`IN_FLIGHT_UNVERIFIED_MAX_AGE_MS`]. A dead or reused pid
/// makes the marker stale, so a plan killed mid-run never wedges the next.
pub fn in_flight_holder_alive(holder: &InFlightPlan, now_ms: i64) -> bool {
    match holder.process_start {
        Some(start) => {
            let live = i32::try_from(holder.pid)
                .ok()
                .and_then(crate::gates::process_start_time);
            live == Some(start)
        }
        None => {
            crate::blockers::process_alive(i64::from(holder.pid))
                && now_ms.saturating_sub(holder.started_at_ms) < IN_FLIGHT_UNVERIFIED_MAX_AGE_MS
        }
    }
}

/// The outcome of claiming the whole-root plan marker.
#[derive(Debug)]
pub enum InFlightClaim {
    /// This plan holds the marker until the guard drops.
    Claimed(InFlightGuard),
    /// Another live plan holds it.
    HeldBy(InFlightPlan),
}

/// Removes the in-flight marker on drop, if it is still this plan's.
#[derive(Debug)]
pub struct InFlightGuard {
    path: PathBuf,
    holder: InFlightPlan,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if read_in_flight(&self.path).as_ref() == Some(&self.holder) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn read_in_flight(path: &Path) -> Option<InFlightPlan> {
    ensure_regular_snapshot(path).ok()?;
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The whole-root plan marker of `root`, if one names a live process.
pub fn in_flight_holder(
    root: &Path,
    alive: &dyn Fn(&InFlightPlan) -> bool,
) -> Option<InFlightPlan> {
    let holder = read_in_flight(&root.join(RECLAIM_IN_FLIGHT_FILENAME))?;
    alive(&holder).then_some(holder)
}

/// Claim the marker that says a whole-root plan of `root` is running.
///
/// Publication is a hard link of a fully written file, so a reader never sees
/// a half-written marker and two claimants cannot both win. A marker naming a
/// process that is gone (or whose pid was reused, or that cannot be parsed)
/// is stale: it is removed and the claim retried once. Two plans that both
/// find the same stale marker can at worst both measure, which costs time and
/// never correctness -- sizes authorize nothing.
pub fn claim_in_flight(
    root: &Path,
    me: InFlightPlan,
    alive: &dyn Fn(&InFlightPlan) -> bool,
) -> io::Result<InFlightClaim> {
    let path = root.join(RECLAIM_IN_FLIGHT_FILENAME);
    let bytes = serde_json::to_vec(&me).map_err(io::Error::other)?;
    for _ in 0..2 {
        if crate::atomic_file::write_synced_if_absent(&path, &bytes)? {
            return Ok(InFlightClaim::Claimed(InFlightGuard { path, holder: me }));
        }
        let observed = std::fs::read(&path).ok();
        let holder = observed
            .as_deref()
            .and_then(|observed| serde_json::from_slice::<InFlightPlan>(observed).ok());
        if let Some(holder) = holder.filter(alive) {
            return Ok(InFlightClaim::HeldBy(holder));
        }
        // Stale. Remove it only if it is still the marker that was judged,
        // and only if it is a regular file this code wrote.
        match ensure_regular_snapshot(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
        if std::fs::read(&path).ok() == observed {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    match read_in_flight(&path) {
        Some(holder) => Ok(InFlightClaim::HeldBy(holder)),
        None => Err(io::Error::other(
            "the whole-root reclaim plan marker kept changing while it was claimed",
        )),
    }
}

/// Wait, within `expired`, for another plan's marker to go away. Returns
/// whether it did. `pause` is called between checks; tests inject one that
/// changes the world instead of sleeping.
pub fn wait_for_in_flight(
    root: &Path,
    alive: &dyn Fn(&InFlightPlan) -> bool,
    expired: &mut dyn FnMut() -> bool,
    pause: &mut dyn FnMut(),
) -> bool {
    loop {
        if in_flight_holder(root, alive).is_none() {
            return true;
        }
        if expired() {
            return false;
        }
        pause();
    }
}

/// Remove the reviewed candidates, re-proving containment for each.
///
/// Re-proving matters because a plan is reviewed as text and applied later;
/// nothing should be deleted on the strength of a path that was only checked
/// before that gap.
pub fn apply(plan: &ReclaimPlan) -> ReclaimOutcome {
    let mut outcome = ReclaimOutcome {
        removed: Vec::new(),
        reclaimed_bytes: 0,
        skipped: Vec::new(),
        not_reviewed: Vec::new(),
    };
    // The plan's "ignored, untracked" evidence came from Git: nothing goes on
    // a cached verdict about that Git (`remove_tree` checks again).
    if plan
        .candidates
        .iter()
        .any(|candidate| candidate.reclaimable)
        && let Err(error) = crate::git::ensure_fresh_git_verdict()
    {
        outcome.skipped.push(format!("nothing removed: {error}"));
        return outcome;
    }
    for candidate in &plan.candidates {
        if !candidate.reclaimable {
            continue;
        }
        if !is_within(&plan.root, &candidate.path) {
            outcome.skipped.push(format!(
                "{} is outside {}",
                candidate.path.display(),
                plan.root.display()
            ));
            continue;
        }
        // A build directory is exactly where a removal walk gets interrupted:
        // it is large, and a tool may still be writing into it. Retrying and
        // then crediting what was actually freed keeps the report truthful --
        // an all-or-nothing removal reports zero bytes for a directory it may
        // have emptied almost completely (#165).
        let result = crate::removal::remove_tree(&candidate.path);
        outcome.reclaimed_bytes += result.freed_bytes;
        if result.removed {
            outcome.removed.push(candidate.path.clone());
        } else if let Some(error) = result.error.as_deref() {
            let progress = if result.is_partial() {
                format!(" ({} byte(s) freed before it failed)", result.freed_bytes)
            } else {
                String::new()
            };
            outcome
                .skipped
                .push(format!("{}: {error}{progress}", candidate.path.display()));
        }
    }
    outcome
}

#[cfg(test)]
mod scan_tests {
    use super::*;

    fn p(value: &str) -> PathBuf {
        PathBuf::from(value)
    }

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.test")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// Make `worktree` its own Git checkout with the given `.gitignore`.
    fn checkout(worktree: &Path, gitignore: &str) {
        std::fs::create_dir_all(worktree).unwrap();
        git(worktree, &["init", "-q"]);
        std::fs::write(worktree.join(".gitignore"), gitignore).unwrap();
    }

    #[test]
    fn a_nested_build_directory_is_found_and_sized() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("session-a");
        checkout(&wt, "target/\n");
        write(&wt.join("packages/app/rust/target/debug/lib.rlib"), 2048);
        write(&wt.join("src/main.rs"), 10);

        let found = scan(tmp.path(), &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].path.ends_with("target"));
        assert_eq!(found[0].bytes, Some(2048));
        assert!(found[0].reclaimable);
    }

    #[test]
    fn an_active_worktree_is_listed_but_not_reclaimable() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("live");
        write(&wt.join("target/x"), 16);
        let found = scan(tmp.path(), std::slice::from_ref(&wt));
        assert_eq!(found.len(), 1);
        assert!(!found[0].reclaimable);
        assert_eq!(reclaimable_bytes(&found), 0);
    }

    /// Nested artefact directories are already inside the parent's total.
    #[test]
    fn scanning_stops_at_the_outermost_build_directory() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("s/target/debug/target/inner"), 4);
        let found = scan(tmp.path(), &[]);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("target"));
    }

    #[test]
    fn configured_artifact_directories_extend_the_built_in_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("s/.aeptus-cache/v3/index"), 16);

        assert!(scan(tmp.path(), &[]).is_empty());
        let found = scan_with_extra_directories(tmp.path(), &[], &[".aeptus-cache".into()]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].path.ends_with(".aeptus-cache"));
        assert_eq!(found[0].bytes, Some(16));
    }

    #[test]
    fn configured_source_and_control_names_are_not_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("s/src/main.rs"), 16);

        let found = scan_with_extra_directories(tmp.path(), &[], &["src".into()]);
        assert!(
            found.is_empty(),
            "configured source roots must stay outside reclaim: {found:?}"
        );
    }

    #[test]
    fn source_directories_are_never_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("s/src/main.rs"), 10);
        write(&tmp.path().join("s/docs/guide.md"), 10);
        assert!(scan(tmp.path(), &[]).is_empty());
    }

    /// The cost being removed: a one-session plan walked every worktree.
    #[test]
    fn a_worktree_scan_never_walks_another_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let mine = tmp.path().join("mine");
        let other = tmp.path().join("other");
        write(&mine.join("target/debug/a"), 8);
        write(&other.join("target/debug/b"), 8);
        write(&other.join("pkg/node_modules/c/index.js"), 8);
        WALKED.with(|walked| walked.borrow_mut().clear());

        let found = scan_worktree(&mine, &[], &[], Sizing::Measure);

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, mine.join("target"));
        assert_eq!(found[0].bytes, Some(8));
        let walked = WALKED.with(|walked| walked.borrow().clone());
        assert!(walked.iter().any(|path| path.starts_with(&mine)));
        let strays: Vec<_> = walked
            .iter()
            .filter(|path| !path.starts_with(&mine))
            .collect();
        assert!(strays.is_empty(), "walked outside the worktree: {strays:?}");
    }

    /// An apply decides from the same candidates without walking their files.
    #[test]
    fn an_unsized_scan_finds_the_same_candidates_without_walking_them() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("s");
        write(&wt.join("target/debug/deep/a"), 64);
        let sized = scan_worktree(&wt, &[], &[], Sizing::Measure);
        WALKED.with(|walked| walked.borrow_mut().clear());

        let unsized_scan = scan_worktree(&wt, &[], &[], Sizing::Skip);

        assert_eq!(decisions(&unsized_scan), decisions(&sized));
        assert_eq!(unsized_scan[0].bytes, None);
        let walked = WALKED.with(|walked| walked.borrow().clone());
        assert!(
            !walked
                .iter()
                .any(|path| path.starts_with(wt.join("target"))),
            "an unsized scan walked into a candidate: {walked:?}"
        );
    }

    #[test]
    fn apply_removes_only_reclaimable_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let done = tmp.path().join("done");
        let live = tmp.path().join("live");
        checkout(&done, "target/\n");
        write(&done.join("target/a"), 64);
        write(&live.join("target/b"), 64);

        let candidates = scan(tmp.path(), std::slice::from_ref(&live));
        let plan = ReclaimPlan {
            digest: "test".into(),
            root: tmp.path().to_path_buf(),
            scope: None,
            reclaimable_bytes: reclaimable_bytes(&candidates),
            total_bytes: measured_bytes(&candidates),
            candidates,
        };
        let outcome = apply(&plan);
        assert_eq!(outcome.removed.len(), 1);
        assert!(!done.join("target").exists(), "inactive artefacts removed");
        assert!(live.join("target").exists(), "active artefacts untouched");
    }

    /// A plan is reviewed as text and applied later; a path that escapes the
    /// root must not be deleted on the strength of a pre-review check.
    #[test]
    fn apply_refuses_a_candidate_outside_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside-target");
        std::fs::create_dir_all(&outside).unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();

        let plan = ReclaimPlan {
            digest: "test".into(),
            root: root.clone(),
            scope: None,
            candidates: vec![classify(&outside, &root, 10, &[])],
            reclaimable_bytes: 10,
            total_bytes: 10,
        };
        let outcome = apply(&plan);
        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert!(outside.exists(), "a path outside the root survives");
    }

    /// A plan is reviewed as text and applied later. Crediting the reviewed
    /// figure would report bytes that were already gone; the apply measures
    /// instead, which is the same mechanism that makes a partially removed
    /// tree credit what it actually freed (#165).
    #[test]
    fn reclaimed_bytes_describe_the_apply_rather_than_the_reviewed_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("s");
        let target = wt.join("target");
        write(&target.join("debug/big.bin"), 4096);
        let candidate = classify(&target, &wt, 4096, &[]);
        assert!(candidate.reclaimable);

        // Between review and apply, a build tool cleared most of it.
        std::fs::remove_file(target.join("debug/big.bin")).unwrap();
        write(&target.join("debug/small.bin"), 16);

        let outcome = apply(&ReclaimPlan {
            digest: "test".into(),
            root: tmp.path().to_path_buf(),
            scope: None,
            candidates: vec![candidate],
            reclaimable_bytes: 4096,
            total_bytes: 4096,
        });
        assert_eq!(outcome.removed, vec![target]);
        assert_eq!(
            outcome.reclaimed_bytes, 16,
            "the reviewed 4096 was credited instead of the 16 bytes on disk"
        );
    }

    #[test]
    fn a_symlink_contributes_nothing_to_a_total_that_justifies_deletion() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("elsewhere");
        write(&real.join("big"), 4096);
        let wt = tmp.path().join("s");
        std::fs::create_dir_all(wt.join("target")).unwrap();
        std::os::unix::fs::symlink(&real, wt.join("target/link")).unwrap();
        let found = scan(tmp.path(), &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bytes, Some(0), "symlinked content is not counted");
    }

    /// Projects commit `dist/` and `build/`. A name match deleted tracked
    /// files in a real worktree; only what Git ignores is disposable.
    #[test]
    fn a_tracked_build_named_directory_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("s");
        checkout(&wt, "target/\n");
        write(&wt.join("plugins/kanban/dist/index.js"), 64);
        git(&wt, &["add", "plugins/kanban/dist/index.js"]);

        let found = scan(tmp.path(), &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(!found[0].reclaimable, "{found:?}");
        assert!(
            found[0].reason.contains("tracks files"),
            "{}",
            found[0].reason
        );
        assert_eq!(reclaimable_bytes(&found), 0);
    }

    /// Untracked output no rule ignores is not known to be build output.
    #[test]
    fn an_unignored_untracked_build_named_directory_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("s");
        checkout(&wt, "target/\n");
        write(&wt.join("build/report.html"), 64);

        let found = scan(tmp.path(), &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(!found[0].reclaimable);
        assert!(
            found[0].reason.contains("does not ignore"),
            "{}",
            found[0].reason
        );
    }

    /// An ignore rule does not make a force-added file disposable.
    #[test]
    fn an_ignored_directory_holding_tracked_files_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("s");
        checkout(&wt, "dist/\n");
        write(&wt.join("dist/vendored.js"), 64);
        git(&wt, &["add", "-f", "dist/vendored.js"]);

        let found = scan(tmp.path(), &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(!found[0].reclaimable);
        assert!(
            found[0].reason.contains("tracks files"),
            "{}",
            found[0].reason
        );
    }

    #[test]
    fn a_directory_that_is_not_its_own_checkout_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("plain/target/x"), 16);

        let found = scan(tmp.path(), &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(!found[0].reclaimable);
        assert!(
            found[0].reason.contains("not a Git checkout"),
            "{}",
            found[0].reason
        );
    }

    #[test]
    fn a_worktree_no_session_records_is_kept() {
        let mut candidates = vec![
            classify(
                &PathBuf::from("/w/known/target"),
                &PathBuf::from("/w/known"),
                8,
                &[],
            ),
            classify(
                &PathBuf::from("/w/orphan/target"),
                &PathBuf::from("/w/orphan"),
                8,
                &[],
            ),
        ];
        protect_unrecorded_worktrees(&mut candidates, &[PathBuf::from("/w/known")]);
        assert!(candidates[0].reclaimable);
        assert!(!candidates[1].reclaimable);
        assert!(
            candidates[1].reason.contains("no session"),
            "{}",
            candidates[1].reason
        );
    }
    #[test]
    fn a_worktree_with_an_open_file_is_kept() {
        let mut candidates = vec![
            classify(&p("/w/busy/target"), &p("/w/busy"), 8, &[]),
            classify(&p("/w/quiet/target"), &p("/w/quiet"), 8, &[]),
        ];
        protect_worktrees_with_open_file_uses(
            &mut candidates,
            &[OpenFileUse {
                pid: 4321,
                command: "node".into(),
                descriptor: "3".into(),
                file_type: Some("REG".into()),
                path: p("/w/busy/web/vite.log"),
            }],
        );
        assert!(!candidates[0].reclaimable);
        assert!(
            candidates[0].reason.contains("4321")
                && candidates[0].reason.contains("node")
                && candidates[0].reason.contains("/w/busy/web/vite.log"),
            "{}",
            candidates[0].reason
        );
        assert!(candidates[1].reclaimable, "{}", candidates[1].reason);
    }

    #[test]
    fn a_sibling_worktree_sharing_a_name_prefix_is_not_kept() {
        let mut candidates = vec![classify(&p("/w/app/target"), &p("/w/app"), 8, &[])];
        protect_worktrees_with_open_file_uses(
            &mut candidates,
            &[OpenFileUse {
                pid: 4321,
                command: "node".into(),
                descriptor: "3".into(),
                file_type: Some("REG".into()),
                path: p("/w/app-2/src/main.rs"),
            }],
        );
        assert!(candidates[0].reclaimable, "{}", candidates[0].reason);
    }

    #[test]
    fn lsof_records_retain_process_identity_and_are_narrowed_to_the_root() {
        let listing = b"p123\0cnode\0\nfcwd\0tDIR\0n/w/root\0\nf3\0tREG\0n/w/root/path\nwith-newline\0\np9\0csh\0\nf4\0tREG\0n/w/other/file\0\n";
        let OpenFileSnapshot::Complete(open_files) =
            parse_lsof_output(true, listing, &[], Path::new("/w/root"))
        else {
            panic!("well-formed lsof records should be complete");
        };
        assert_eq!(open_files.len(), 2);
        assert_eq!(open_files[0].pid, 123);
        assert_eq!(open_files[0].command, "node");
        assert_eq!(open_files[0].descriptor, "cwd");
        assert_eq!(open_files[0].file_type.as_deref(), Some("DIR"));
        assert_eq!(open_files[0].path, p("/w/root"));
        assert_eq!(open_files[1].pid, 123);
        assert_eq!(open_files[1].descriptor, "3");
        assert_eq!(open_files[1].file_type.as_deref(), Some("REG"));
        assert_eq!(open_files[1].path, p("/w/root/path\nwith-newline"));

        let without_final_linefeed = &listing[..listing.len() - 1];
        assert!(matches!(
            parse_lsof_output(true, without_final_linefeed, &[], Path::new("/w/root")),
            OpenFileSnapshot::Complete(_)
        ));
    }

    #[test]
    fn native_empty_names_are_accepted_only_for_known_pathless_descriptor_types() {
        // macOS lsof emits these exact `f`, `t`, and empty `n` fields for
        // NPOLICY descriptors; NEXUS and PIPE use the same pathless shape.
        let listing = b"p123\0cfixture\0\nfcwd\0tDIR\0n/w/root\0\nf10\0tNPOLICY\0n\0\nf11\0tNEXUS\0n\0\nf12\0tPIPE\0n\0\nf13\0tREG\0n/w/root/target/file\0\n";
        let open_files = match parse_lsof_output(true, listing, &[], Path::new("/w/root")) {
            OpenFileSnapshot::Complete(open_files) => open_files,
            other => panic!("known pathless descriptors should not invalidate lsof: {other:?}"),
        };
        assert_eq!(open_files.len(), 2);
        assert_eq!(open_files[0].path, p("/w/root"));
        assert_eq!(open_files[1].path, p("/w/root/target/file"));
    }

    #[test]
    fn unknown_or_filesystem_empty_names_remain_incomplete() {
        let root = Path::new("/w/root");
        let prefix = b"p123\0cfixture\0\nfcwd\0tDIR\0n/w/root\0\n";
        for suffix in [
            &b"f10\0tREG\0n\0\n"[..],
            &b"f10\0tDIR\0n\0\n"[..],
            &b"f10\0tCHR\0n\0\n"[..],
            &b"f10\0n\0\n"[..],
            &b"f10\0tFUTURETYPE\0n\0\n"[..],
        ] {
            let mut listing = prefix.to_vec();
            listing.extend_from_slice(suffix);
            let OpenFileSnapshot::Incomplete { reason, observed } =
                parse_lsof_output(true, &listing, &[], root)
            else {
                panic!("an unclassifiable empty name must remain incomplete");
            };
            assert!(reason.contains("empty field value"), "{reason}");
            assert_eq!(observed.len(), 1, "the complete earlier path is retained");
            assert_eq!(observed[0].path, p("/w/root"));
        }
    }

    #[test]
    fn failed_or_incomplete_lsof_records_are_not_an_empty_snapshot() {
        let root = Path::new("/w/root");
        let partial = b"p123\0cnode\0\nf3\0n/w/root/target/file\0";
        for snapshot in [
            parse_lsof_output(false, partial, &[], root),
            parse_lsof_output(true, partial, b"permission warning", root),
            parse_lsof_output(true, &[], &[], root),
            parse_lsof_output(true, b"p123\0cnode\0n/w/root/target/file", &[], root),
            parse_lsof_output(
                true,
                b"p123\0cnode\0\nf3\0n/w/root/target/file\0\n\n",
                &[],
                root,
            ),
            parse_lsof_output(true, b"p123\0n/w/root/target/file\0", &[], root),
            parse_lsof_output(true, b"cnode\0\nf3\0n/w/root/target/file\0", &[], root),
        ] {
            assert!(
                matches!(&snapshot, OpenFileSnapshot::Incomplete { .. }),
                "invalid lsof result must remain unknown: {snapshot:?}"
            );
        }
    }

    #[test]
    fn lsof_timeout_and_output_limit_remain_unknown() {
        let root = Path::new("/w/root");
        for result in [
            crate::bounded_output::LimitedOutput::Deadline,
            crate::bounded_output::LimitedOutput::OutputLimit,
        ] {
            assert!(matches!(
                snapshot_from_lsof_capture(result, root),
                OpenFileSnapshot::Incomplete { observed, .. } if observed.is_empty()
            ));
        }
    }

    #[test]
    fn incomplete_lsof_retains_known_process_use_without_trusting_absence() {
        let root = Path::new("/w/root");
        let listing = b"p123\0cnode\0\nf3\0n/w/root/target/file\0";
        let OpenFileSnapshot::Incomplete { reason, observed } =
            parse_lsof_output(false, listing, &[], root)
        else {
            panic!("a nonzero lsof exit must be incomplete");
        };
        assert_eq!(observed.len(), 1);
        let mut candidates = vec![
            classify(&p("/w/root/target"), &p("/w/root"), 8, &[]),
            classify(&p("/w/other/target"), &p("/w/other"), 8, &[]),
        ];
        protect_worktrees_when_open_file_snapshot_is_incomplete(
            &mut candidates,
            &reason,
            &observed,
        );
        assert!(!candidates[0].reclaimable);
        assert!(candidates[0].reason.contains("123"));
        assert!(candidates[0].reason.contains("node"));
        assert!(candidates[0].reason.contains("/w/root/target/file"));
        assert!(candidates[0].reason.contains("incomplete"));
        assert!(!candidates[1].reclaimable);
        assert!(
            candidates[1]
                .reason
                .contains("process use could not be checked")
        );
    }

    #[test]
    fn incomplete_process_use_keeps_every_otherwise_reclaimable_candidate() {
        let mut candidates = vec![
            classify(&p("/w/one/target"), &p("/w/one"), 8, &[]),
            classify(&p("/w/two/node_modules"), &p("/w/two"), 8, &[]),
        ];
        protect_worktrees_when_open_file_snapshot_is_incomplete(
            &mut candidates,
            "lsof exceeded its time limit",
            &[],
        );
        assert!(candidates.iter().all(|candidate| !candidate.reclaimable));
        assert!(candidates.iter().all(|candidate| {
            candidate
                .reason
                .contains("process use could not be checked")
                && candidate.reason.contains("kept for safety")
        }));
    }

    #[test]
    fn open_process_reason_includes_bounded_pid_command_and_path() {
        let mut candidates = vec![
            classify(&p("/w/busy/target"), &p("/w/busy"), 8, &[]),
            classify(&p("/w/quiet/target"), &p("/w/quiet"), 8, &[]),
        ];
        protect_worktrees_with_open_file_uses(
            &mut candidates,
            &[OpenFileUse {
                pid: 4321,
                command: "node\nwatcher".into(),
                descriptor: "3".into(),
                file_type: Some("REG".into()),
                path: p("/w/busy/node_modules/.pnpm-store/index"),
            }],
        );
        assert!(!candidates[0].reclaimable);
        assert!(candidates[0].reason.contains("4321"));
        assert!(candidates[0].reason.contains("node\\nwatcher"));
        assert!(candidates[0].reason.contains(".pnpm-store/index"));
        assert!(candidates[0].reason.len() <= 520);
        assert!(candidates[1].reclaimable);
    }

    #[test]
    fn process_use_blocker_keeps_unknown_results_and_attributes_known_ones() {
        let worktree = p("/w/repo");
        let open = OpenFileUse {
            pid: 77,
            command: "node".into(),
            descriptor: "3".into(),
            file_type: Some("REG".into()),
            path: p("/w/repo/node_modules/.pnpm-store/index"),
        };
        let complete = OpenFileSnapshot::Complete(vec![open.clone()]);
        let reason = open_file_snapshot_blocker_for_worktree(&complete, &worktree).unwrap();
        assert!(reason.contains("77"));
        assert!(reason.contains("node"));
        assert!(reason.contains(".pnpm-store/index"));

        let incomplete_without_names = OpenFileSnapshot::Incomplete {
            reason: "lsof exceeded its time limit".into(),
            observed: Vec::new(),
        };
        let reason =
            open_file_snapshot_blocker_for_worktree(&incomplete_without_names, &worktree).unwrap();
        assert!(reason.contains("process use could not be checked"));
        assert!(reason.contains("kept for safety"));

        let incomplete_with_known_use = OpenFileSnapshot::Incomplete {
            reason: "lsof exited unsuccessfully".into(),
            observed: vec![open],
        };
        let reason =
            open_file_snapshot_blocker_for_worktree(&incomplete_with_known_use, &worktree).unwrap();
        assert!(reason.contains("77"));
        assert!(reason.contains("/w/repo/node_modules/.pnpm-store/index"));
        assert!(reason.contains("incomplete"));
        assert!(reason.contains("kept for safety"));
    }

    #[test]
    fn only_the_live_holders_exact_worktree_cwd_is_allowed_for_metadata_close() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path();
        let cwd = OpenFileUse {
            pid: 77,
            command: "agent".into(),
            descriptor: "cwd".into(),
            file_type: Some("DIR".into()),
            path: std::fs::canonicalize(worktree).unwrap(),
        };
        let complete = OpenFileSnapshot::Complete(vec![cwd.clone()]);
        assert_eq!(
            open_file_snapshot_blocker_for_session_worktree(&complete, worktree, Some(77)),
            None
        );
        assert!(
            open_file_snapshot_blocker_for_session_worktree(&complete, worktree, Some(78))
                .is_some()
        );

        let open_file = OpenFileUse {
            descriptor: "3".into(),
            file_type: Some("REG".into()),
            path: worktree.join("notes.txt"),
            ..cwd.clone()
        };
        assert!(
            open_file_snapshot_blocker_for_session_worktree(
                &OpenFileSnapshot::Complete(vec![cwd.clone(), open_file]),
                worktree,
                Some(77)
            )
            .is_some()
        );
        assert!(
            open_file_snapshot_blocker_for_session_worktree(
                &OpenFileSnapshot::Incomplete {
                    reason: "partial process listing".into(),
                    observed: vec![cwd],
                },
                worktree,
                Some(77)
            )
            .is_some()
        );
    }

    fn set_index_age(worktree: &Path, age: std::time::Duration) {
        let index = git_index_path(worktree).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(index).unwrap();
        file.set_modified(std::time::SystemTime::now() - age)
            .unwrap();
    }

    #[test]
    fn a_worktree_worked_in_recently_is_kept_and_an_idle_one_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        let recent = tmp.path().join("recent");
        let idle = tmp.path().join("idle");
        for worktree in [&recent, &idle] {
            checkout(worktree, "target/\n");
            git(worktree, &["add", ".gitignore"]);
        }
        set_index_age(&recent, std::time::Duration::from_secs(20 * 60));
        set_index_age(&idle, std::time::Duration::from_secs(4 * 3600));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let mut candidates = vec![
            classify(&recent.join("target"), &recent, 8, &[]),
            classify(&idle.join("target"), &idle, 8, &[]),
        ];
        protect_recently_worked_worktrees(&mut candidates, now, RECENT_WORK_WINDOW_MS);
        assert!(!candidates[0].reclaimable, "{}", candidates[0].reason);
        assert!(
            candidates[0].reason.contains("Git index changed"),
            "{}",
            candidates[0].reason
        );
        assert!(candidates[1].reclaimable, "{}", candidates[1].reason);
    }

    #[test]
    fn a_linked_worktrees_index_is_found_through_its_gitdir_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        checkout(&main, "target/\n");
        git(&main, &["add", ".gitignore"]);
        git(&main, &["commit", "-qm", "init"]);
        let linked = tmp.path().join("linked");
        git(&main, &["worktree", "add", "-q", linked.to_str().unwrap()]);
        let index = git_index_path(&linked).unwrap();
        assert!(index.exists(), "{}", index.display());
        assert_ne!(index, main.join(".git/index"));
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    /// One kept and one reclaimable candidate, each holding one file.
    fn two_candidates(root: &Path) -> Vec<ReclaimCandidate> {
        write(&root.join("kept/target/x"), 16);
        write(&root.join("idle/target/x"), 32);
        vec![
            classify(
                &root.join("kept/target"),
                &root.join("kept"),
                0,
                &[root.join("kept")],
            ),
            classify(&root.join("idle/target"), &root.join("idle"), 0, &[]),
        ]
    }

    fn measure(
        candidates: &mut [ReclaimCandidate],
        expired: &mut dyn FnMut() -> bool,
    ) -> (SizingReport, Vec<(PathBuf, u64)>) {
        measure_within(candidates, &BTreeMap::new(), true, expired)
    }

    fn holder(pid: u32) -> InFlightPlan {
        InFlightPlan {
            pid,
            process_start: Some(u64::from(pid)),
            started_at_ms: i64::from(pid),
        }
    }

    fn claimed(claim: InFlightClaim) -> InFlightGuard {
        match claim {
            InFlightClaim::Claimed(guard) => guard,
            InFlightClaim::HeldBy(holder) => panic!("expected a claim, found {holder:?}"),
        }
    }

    #[test]
    fn a_spent_budget_lists_every_candidate_as_unmeasured_not_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let mut candidates = two_candidates(tmp.path());

        let (report, fresh) = measure(&mut candidates, &mut || true);

        assert_eq!(candidates.len(), 2, "unmeasured candidates stay listed");
        assert!(candidates.iter().all(|candidate| candidate.bytes.is_none()));
        assert_eq!(report.unmeasured, 2);
        assert_eq!(report.measured, 0);
        assert!(report.budget_exhausted);
        assert!(fresh.is_empty());
        assert_eq!(reclaimable_bytes(&candidates), 0);
        let json = serde_json::to_value(&candidates[0]).unwrap();
        assert!(
            json["bytes"].is_null(),
            "unmeasured must not read as 0: {json}"
        );
    }

    #[test]
    fn sizing_stops_mid_way_after_measuring_reclaimable_candidates_first() {
        let tmp = tempfile::tempdir().unwrap();
        let mut candidates = two_candidates(tmp.path());
        // One check before the reclaimable directory and one for its single
        // file; the third check, before the kept directory, finds the budget
        // spent.
        let mut checks = 0;
        let mut expired = || {
            checks += 1;
            checks > 2
        };

        let (report, fresh) = measure(&mut candidates, &mut expired);

        let idle = candidates
            .iter()
            .find(|candidate| candidate.reclaimable)
            .unwrap();
        let kept = candidates
            .iter()
            .find(|candidate| !candidate.reclaimable)
            .unwrap();
        assert_eq!(idle.bytes, Some(32));
        assert_eq!(kept.bytes, None);
        assert_eq!((report.measured, report.unmeasured), (1, 1));
        assert!(report.budget_exhausted);
        assert_eq!(fresh, vec![(tmp.path().join("idle/target"), 32)]);
        assert_eq!(candidates[0].path, idle.path, "measured sizes sort first");
    }

    #[test]
    fn an_unbounded_budget_measures_every_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let mut candidates = two_candidates(tmp.path());

        let (report, fresh) = measure(&mut candidates, &mut || false);

        assert_eq!((report.measured, report.unmeasured), (2, 0));
        assert!(!report.budget_exhausted);
        assert_eq!(fresh.len(), 2);
        assert_eq!(measured_bytes(&candidates), 48);
    }

    #[test]
    fn a_recent_measurement_is_reused_without_walking_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mut candidates = two_candidates(tmp.path());
        let recent = BTreeMap::from([
            (tmp.path().join("idle/target"), 7_u64),
            (tmp.path().join("kept/target"), 9_u64),
        ]);
        WALKED.with(|walked| walked.borrow_mut().clear());

        let (report, fresh) = measure_within(&mut candidates, &recent, true, &mut || false);

        assert_eq!((report.reused, report.measured), (2, 0));
        assert!(fresh.is_empty(), "a reused size is not re-recorded as new");
        assert_eq!(measured_bytes(&candidates), 16);
        assert!(WALKED.with(|walked| walked.borrow().is_empty()));
    }

    /// While another plan measures the root, this one walks nothing.
    #[test]
    fn a_deferring_plan_never_walks() {
        let tmp = tempfile::tempdir().unwrap();
        let mut candidates = two_candidates(tmp.path());
        WALKED.with(|walked| walked.borrow_mut().clear());

        let (report, _) = measure_within(&mut candidates, &BTreeMap::new(), false, &mut || false);

        assert_eq!(report.unmeasured, 2);
        assert!(
            !report.budget_exhausted,
            "nothing ran out; nothing was tried"
        );
        assert!(WALKED.with(|walked| walked.borrow().is_empty()));
    }

    /// Sizes are not authorization-bearing: measuring nothing and measuring
    /// everything confirm the same decision.
    #[test]
    fn unmeasured_candidates_leave_the_digest_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let mut unmeasured = two_candidates(tmp.path());
        let mut measured = unmeasured.clone();
        measure(&mut unmeasured, &mut || true);
        measure(&mut measured, &mut || false);

        assert_eq!(
            plan_digest(tmp.path(), None, &unmeasured),
            plan_digest(tmp.path(), None, &measured)
        );
    }

    /// A review whose sizes were not measured still authorizes exactly the
    /// paths it listed reclaimable, and nothing it did not list.
    #[test]
    fn an_unmeasured_review_authorizes_only_the_paths_it_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reviewed = two_candidates(tmp.path());
        measure(&mut reviewed, &mut || true);
        let mut fresh = reviewed.clone();
        write(&tmp.path().join("new/target/x"), 8);
        fresh.push(classify(
            &tmp.path().join("new/target"),
            &tmp.path().join("new"),
            8,
            &[],
        ));

        let scope = restrict_to_review(&fresh, &decisions(&reviewed));

        let authorized: Vec<_> = scope
            .candidates
            .iter()
            .filter(|candidate| candidate.reclaimable)
            .map(|candidate| candidate.path.clone())
            .collect();
        assert_eq!(authorized, vec![tmp.path().join("idle/target")]);
        assert_eq!(scope.not_reviewed, vec![tmp.path().join("new/target")]);
    }

    /// A cut-off candidate list is never a whole-root review: its digest
    /// matches no complete scan, and it cannot be saved as one.
    #[test]
    fn a_partial_plan_digest_is_never_a_complete_review() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let candidates = vec![classify(&root.join("a/target"), &root.join("a"), 8, &[])];
        let partial = partial_plan_digest(&root, &candidates);
        let scoped = plan_digest(&root, Some(&root.join("a")), &candidates);

        assert_ne!(partial, plan_digest(&root, None, &candidates));
        assert_ne!(partial, scoped);
        assert!(save_snapshot(&root, None, &partial, &candidates).is_err());
        assert_eq!(load_snapshot(&root, &partial).unwrap(), None);
    }

    #[test]
    fn discovery_that_runs_out_of_budget_says_it_is_partial() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            write(&tmp.path().join(name).join("target/x"), 4);
        }

        let (none, spent) = scan_root_within(tmp.path(), &[], &[], Sizing::Skip, &mut || true);
        assert!(none.is_empty());
        assert!(!spent.complete);
        assert_eq!(spent.worktrees_scanned, 0);
        assert_eq!(spent.worktrees_not_scanned, 3);

        let mut checks = 0;
        let mut expired = || {
            checks += 1;
            checks > 1
        };
        let (one, partial) = scan_root_within(tmp.path(), &[], &[], Sizing::Skip, &mut expired);
        assert_eq!(one.len(), 1, "{one:?}");
        assert_eq!(one[0].path, tmp.path().join("a/target"), "name order");
        assert!(!partial.complete);
        assert_eq!(partial.worktrees_scanned, 1);
        assert_eq!(partial.worktrees_not_scanned, 2);

        let (all, complete) = scan_root_within(tmp.path(), &[], &[], Sizing::Skip, &mut || false);
        assert_eq!(all.len(), 3);
        assert!(complete.complete);
        assert_eq!(complete.worktrees_not_scanned, 0);
        assert!(all.iter().all(|candidate| candidate.bytes.is_none()));
    }

    #[test]
    fn recorded_sizes_are_reused_only_inside_the_window_and_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let inside = root.join("a/target");
        let outside = tmp.path().join("elsewhere/target");
        let window = SIZE_REUSE_WINDOW_MS;
        let measured = [(inside.clone(), 5), (outside, 6)];

        record_sizes(&root, 1_000, window, &measured).unwrap();

        assert_eq!(
            recent_sizes(&root, 1_000 + window, window),
            BTreeMap::from([(inside.clone(), 5)])
        );
        assert!(
            recent_sizes(&root, 1_001 + window, window).is_empty(),
            "too old"
        );
        assert!(
            recent_sizes(&root, 999, window).is_empty(),
            "from the future"
        );
        let moved = tmp.path().join("moved");
        std::fs::rename(&root, &moved).unwrap();
        assert!(
            recent_sizes(&moved, 1_000, window).is_empty(),
            "another root's measurements are not this root's"
        );
    }

    #[test]
    fn rerecording_keeps_an_older_measurement_at_its_original_time() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let first = root.join("a/target");
        let second = root.join("b/target");
        let window = SIZE_REUSE_WINDOW_MS;
        record_sizes(root, 1_000, window, &[(first.clone(), 5)]).unwrap();
        record_sizes(root, 2_000, window, &[(second.clone(), 6)]).unwrap();

        assert_eq!(
            recent_sizes(root, 1_000 + window, window),
            BTreeMap::from([(first, 5), (second.clone(), 6)])
        );
        assert_eq!(
            recent_sizes(root, 2_000 + window, window),
            BTreeMap::from([(second, 6)])
        );
    }

    #[test]
    fn a_live_in_flight_plan_is_reported_and_its_marker_removed_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let guard = claimed(claim_in_flight(root, holder(1), &|_| true).unwrap());

        match claim_in_flight(root, holder(2), &|_| true).unwrap() {
            InFlightClaim::HeldBy(found) => assert_eq!(found, holder(1)),
            InFlightClaim::Claimed(_) => panic!("a live plan's marker must not be taken"),
        }
        assert_eq!(in_flight_holder(root, &|_| true), Some(holder(1)));

        drop(guard);
        assert_eq!(in_flight_holder(root, &|_| true), None);
        assert!(matches!(
            claim_in_flight(root, holder(2), &|_| true).unwrap(),
            InFlightClaim::Claimed(_)
        ));
    }

    /// A plan killed mid-run leaves its marker; the next plan heals it.
    #[test]
    fn a_stale_in_flight_marker_self_heals() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let stale = claimed(claim_in_flight(root, holder(1), &|_| true).unwrap());
        let alive = |plan: &InFlightPlan| plan.pid != 1;

        let fresh = claimed(claim_in_flight(root, holder(2), &alive).unwrap());
        // The dead plan's guard must not remove the marker that replaced it.
        drop(stale);
        assert_eq!(in_flight_holder(root, &alive), Some(holder(2)));
        drop(fresh);

        std::fs::write(root.join(RECLAIM_IN_FLIGHT_FILENAME), b"not json").unwrap();
        assert!(matches!(
            claim_in_flight(root, holder(3), &|_| true).unwrap(),
            InFlightClaim::Claimed(_)
        ));
    }

    #[test]
    fn liveness_requires_the_same_process_start_time() {
        let now = 1_000;
        assert!(in_flight_holder_alive(&InFlightPlan::current(now), now));
        let reused_pid = InFlightPlan {
            process_start: Some(u64::MAX),
            ..InFlightPlan::current(now)
        };
        assert!(!in_flight_holder_alive(&reused_pid, now));
    }

    #[test]
    fn waiting_for_an_in_flight_plan_ends_when_it_finishes_or_the_budget_does() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let guard = claimed(claim_in_flight(&root, holder(1), &|_| true).unwrap());

        let mut pauses = 0;
        let finished = wait_for_in_flight(&root, &|_| true, &mut || true, &mut || pauses += 1);
        assert!(!finished);
        assert_eq!(pauses, 0, "a spent budget does not wait");

        let mut guard = Some(guard);
        let mut release = || drop(guard.take());
        let finished = wait_for_in_flight(&root, &|_| true, &mut || false, &mut release);
        assert!(finished);
        assert!(guard.is_none());
    }
}
