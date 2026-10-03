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

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const RECLAIM_PLAN_SCHEMA_VERSION: u8 = 1;
const RECLAIM_PLAN_FILENAME_PREFIX: &str = ".aethyme-reclaim-plan-";
const RECLAIM_PLAN_FILENAME_SUFFIX: &str = ".json";

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
    let mut hasher = Sha256::new();
    hasher.update(b"aethyme-reclaim-plan-v1\0");
    hasher.update(root.as_os_str().as_encoded_bytes());
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
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reclaim plan digest must be a 64-character hexadecimal SHA-256",
        ));
    }
    Ok(root.join(format!(
        "{RECLAIM_PLAN_FILENAME_PREFIX}{digest}{RECLAIM_PLAN_FILENAME_SUFFIX}"
    )))
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
    pub bytes: u64,
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
        bytes,
        reclaimable: !is_active,
        reason: if is_active {
            "session is still active; a build may be running against it".into()
        } else {
            "regenerable build output for an inactive session".into()
        },
    }
}

/// Total bytes the reclaimable candidates would free.
pub fn reclaimable_bytes(candidates: &[ReclaimCandidate]) -> u64 {
    candidates
        .iter()
        .filter(|candidate| candidate.reclaimable)
        .map(|candidate| candidate.bytes)
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
        assert_eq!(candidate.bytes, 7 << 30);
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
    let mut total = 0;
    let Ok(entries) = read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            total += directory_bytes(&entry.path());
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
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
    let mut found = Vec::new();
    let Ok(worktrees) = std::fs::read_dir(root) else {
        return found;
    };
    for worktree in worktrees.flatten() {
        let base = worktree.path();
        if !base.is_dir() {
            continue;
        }
        found.extend(scan_worktree(&base, active, extras, sizing));
    }
    found.sort_by_key(|candidate| std::cmp::Reverse(candidate.bytes));
    found
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

/// Keep candidates in worktrees a process has a file or working directory
/// open in.
///
/// Session state cannot see a dev server, a test watcher or a build started
/// from a plain shell: each outlives the agent that launched it and keeps
/// writing to the very `node_modules` or `target/` a reclaim would delete.
/// `open_paths` is one snapshot of every open path (see
/// [`open_paths_under`]); a candidate is kept when any of them lies in its
/// worktree.
pub fn protect_worktrees_with_open_files(
    candidates: &mut [ReclaimCandidate],
    open_paths: &[PathBuf],
) {
    for candidate in candidates.iter_mut().filter(|c| c.reclaimable) {
        let worktree = &candidate.worktree;
        let canonical = std::fs::canonicalize(worktree).ok();
        let in_use = open_paths.iter().any(|open| {
            open.starts_with(worktree)
                || canonical
                    .as_ref()
                    .is_some_and(|canonical| open.starts_with(canonical))
        });
        if in_use {
            candidate.reclaimable = false;
            candidate.reason = "a process has a file or working directory open in this \
                worktree; a build or dev server may still be using it"
                .into();
        }
    }
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

/// Every path a process on this host has open beneath `root`, from one
/// `lsof` snapshot, or `None` when the snapshot cannot be taken.
///
/// One system-wide listing filtered here, rather than `lsof +D`: `+D` walks
/// the whole tree, which under a root of build caches is the slow part of a
/// reclaim already. Working directories are included -- `lsof` reports them as
/// the `cwd` descriptor -- so an idle shell parked in a worktree keeps it.
pub fn open_paths_under(root: &Path) -> Option<Vec<PathBuf>> {
    let output = std::process::Command::new("lsof")
        .args(["-w", "-n", "-P", "-Fn"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    // `lsof` exits 1 when it could not inspect some process (another user's,
    // one that exited mid-listing) while still listing every other one; only
    // an empty listing means no snapshot was taken.
    if output.stdout.is_empty() {
        return None;
    }
    Some(parse_lsof_names(
        &String::from_utf8_lossy(&output.stdout),
        root,
    ))
}

/// The `n` (name) fields of `lsof -F` output that lie beneath `root`, under
/// either its given or its canonical spelling.
fn parse_lsof_names(listing: &str, root: &Path) -> Vec<PathBuf> {
    let canonical = std::fs::canonicalize(root).ok();
    listing
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .map(PathBuf::from)
        .filter(|path| {
            path.starts_with(root)
                || canonical
                    .as_ref()
                    .is_some_and(|canonical| path.starts_with(canonical))
        })
        .collect()
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
                let bytes = match self.sizing {
                    Sizing::Measure => directory_bytes(&path),
                    Sizing::Skip => 0,
                };
                let mut candidate = classify(&path, self.worktree, bytes, self.active);
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
        assert_eq!(found[0].bytes, 2048);
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
        assert_eq!(found[0].bytes, 16);
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
        assert_eq!(found[0].bytes, 8);
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
        assert_eq!(unsized_scan[0].bytes, 0);
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
            total_bytes: candidates.iter().map(|c| c.bytes).sum(),
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
        assert_eq!(found[0].bytes, 0, "symlinked content is not counted");
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
        protect_worktrees_with_open_files(&mut candidates, &[p("/w/busy/web/vite.log")]);
        assert!(!candidates[0].reclaimable);
        assert!(
            candidates[0].reason.contains("open"),
            "{}",
            candidates[0].reason
        );
        assert!(candidates[1].reclaimable, "{}", candidates[1].reason);
    }

    #[test]
    fn a_sibling_worktree_sharing_a_name_prefix_is_not_kept() {
        let mut candidates = vec![classify(&p("/w/app/target"), &p("/w/app"), 8, &[])];
        protect_worktrees_with_open_files(&mut candidates, &[p("/w/app-2/src/main.rs")]);
        assert!(candidates[0].reclaimable, "{}", candidates[0].reason);
    }

    #[test]
    fn lsof_names_are_narrowed_to_the_root() {
        let listing = "p123\nfcwd\nn/w/root/a\nf3\nn/elsewhere/b\np9\nn/w/root/c/d.txt\n";
        assert_eq!(
            parse_lsof_names(listing, Path::new("/w/root")),
            vec![p("/w/root/a"), p("/w/root/c/d.txt")]
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
