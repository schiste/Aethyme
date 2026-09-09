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

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Digest over exactly what the operator reads: each reclaimable path and its
/// size. A candidate appearing, vanishing or changing size invalidates the
/// plan, because the reviewed total is no longer the one being applied.
pub fn plan_digest(root: &Path, candidates: &[ReclaimCandidate]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root.as_os_str().as_encoded_bytes());
    for candidate in candidates {
        hasher.update(b"\n");
        hasher.update(candidate.path.as_os_str().as_encoded_bytes());
        hasher.update(format!(":{}:{}", candidate.bytes, candidate.reclaimable).as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Directory names that hold regenerable build output.
///
/// A fixed list rather than a heuristic. "Large and ignored" would also match a
/// downloaded dataset or a local database someone cannot rebuild, and this
/// deletes things.
const ARTEFACT_DIRECTORIES: &[&str] = &["target", "node_modules", ".venv", "build", "dist"];

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

/// Whether a directory name is regenerable build output.
pub fn is_artefact_directory(name: &str) -> bool {
    ARTEFACT_DIRECTORIES.contains(&name)
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
    let is_active = active.iter().any(|candidate| candidate == worktree);
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
        for name in ["target", "node_modules", ".venv", "build", "dist"] {
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

    #[test]
    fn only_reclaimable_candidates_count_toward_the_total() {
        let candidates = vec![
            classify(&p("/w/a/target"), &p("/w/a"), 100, &[]),
            classify(&p("/w/b/target"), &p("/w/b"), 200, &[p("/w/b")]),
        ];
        assert_eq!(reclaimable_bytes(&candidates), 100);
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
}

/// Directory size, following no symlinks.
///
/// A symlink into someone else's tree must contribute nothing to a total that
/// is about to justify a deletion.
pub fn directory_bytes(path: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(path) else {
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
    let mut found = Vec::new();
    let Ok(worktrees) = std::fs::read_dir(root) else {
        return found;
    };
    for worktree in worktrees.flatten() {
        let base = worktree.path();
        if !base.is_dir() {
            continue;
        }
        collect(&base, &base, active, &mut found, 0);
    }
    found.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    found
}

fn collect(
    dir: &Path,
    worktree: &Path,
    active: &[PathBuf],
    found: &mut Vec<ReclaimCandidate>,
    depth: usize,
) {
    // Build trees are shallow relative to a repository; this bounds the walk on
    // a directory whose whole problem is that it is enormous.
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
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
        if is_artefact_directory(name) {
            let bytes = directory_bytes(&path);
            found.push(classify(&path, worktree, bytes, active));
            continue;
        }
        collect(&path, worktree, active, found, depth + 1);
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

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn a_nested_build_directory_is_found_and_sized() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("session-a");
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
        let found = scan(tmp.path(), &[wt.clone()]);
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
    fn source_directories_are_never_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("s/src/main.rs"), 10);
        write(&tmp.path().join("s/docs/guide.md"), 10);
        assert!(scan(tmp.path(), &[]).is_empty());
    }

    #[test]
    fn apply_removes_only_reclaimable_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let done = tmp.path().join("done");
        let live = tmp.path().join("live");
        write(&done.join("target/a"), 64);
        write(&live.join("target/b"), 64);

        let candidates = scan(tmp.path(), &[live.clone()]);
        let plan = ReclaimPlan {
            digest: "test".into(),
            root: tmp.path().to_path_buf(),
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
}
