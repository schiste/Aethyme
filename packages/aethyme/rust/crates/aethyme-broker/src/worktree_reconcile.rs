//! Reconciling what is on disk against what the broker believes it owns.
//!
//! The broker's cleanup lanes all start from session rows, so a directory
//! under a worktree root that no row references is invisible to every one of
//! them. On the machine that motivated #176 that was roughly a third of the
//! directories -- 54 on disk against 38 claimed -- and no retention policy
//! could reach a byte of it. Ownership drift is not a cleanup problem; it is a
//! *knowledge* problem, and the first thing to fix is that nobody can see it.
//!
//! This sweep reports. It never removes anything and nothing downstream of it
//! may: a directory the broker has no record of is, by definition, a directory
//! whose contents the broker cannot reason about. Naming the drift is what
//! lets a human act on it; acting on it automatically would be guessing with
//! somebody else's work.
//!
//! Classification is pure and takes already-normalised paths. Deciding that
//! two paths are the same file is a filesystem question (symlinks,
//! `/var` against `/private/var`, trailing separators) and belongs where the
//! filesystem is, not in the matching rule.

/// One directory observed directly beneath a broker-owned worktree root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedDirectory {
    /// Normalised absolute path.
    pub path: String,
    /// Whether the directory carries a `.git` entry, i.e. looks like a git
    /// worktree rather than a stray directory.
    pub git_marker: bool,
    /// `None` when the sweep ran without sizing. Sizing walks the whole tree
    /// and is the expensive half of this; the counts are useful without it.
    pub estimated_bytes: Option<u64>,
}

/// Whether a session row accounts for a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryClaim {
    Claimed { session_id: i64 },
    Unclaimed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledDirectory {
    pub path: String,
    pub claim: DirectoryClaim,
    pub git_marker: bool,
    pub estimated_bytes: Option<u64>,
}

impl ReconciledDirectory {
    pub fn unclaimed(&self) -> bool {
        self.claim == DirectoryClaim::Unclaimed
    }

    /// How the directory should be described to someone deciding what to do
    /// with it. A former worktree and a stray directory want different
    /// handling, and the `.git` marker is the only evidence available without
    /// reading the contents.
    pub fn kind(&self) -> &'static str {
        if self.git_marker {
            "untracked_worktree"
        } else {
            "stray_directory"
        }
    }
}

pub const WORKTREE_RECONCILIATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct WorktreeReconciliation {
    pub schema_version: u32,
    /// Broker-owned roots actually scanned. Zero means the sweep found no root
    /// to look at, which is different from finding a root that was empty.
    pub scanned_root_count: usize,
    pub directory_count: usize,
    pub claimed_count: usize,
    pub unclaimed_count: usize,
    /// Sum over unclaimed directories that were sized. `0` with a non-zero
    /// `unclaimed_count` means the sweep ran without sizing, not that the
    /// directories are empty -- read `sized` before reporting this as a total.
    pub unclaimed_bytes: u64,
    /// Whether every unclaimed directory carries a byte estimate.
    pub sized: bool,
    pub unclaimed: Vec<UnclaimedDirectory>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct UnclaimedDirectory {
    pub path: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
}

/// Match observed directories against the paths sessions claim.
///
/// `claims` carries every session the broker knows about, closed ones
/// included. A closed session whose worktree still exists is accounted for --
/// the cleanup lane owns that case, and reporting it here as drift would send
/// someone chasing a directory that is already on a policy's list.
pub fn reconcile(
    observed: &[ObservedDirectory],
    claims: &[(i64, String)],
) -> Vec<ReconciledDirectory> {
    observed
        .iter()
        .map(|directory| {
            let claim = claims
                .iter()
                .find(|(_, path)| path == &directory.path)
                .map(|(session_id, _)| DirectoryClaim::Claimed {
                    session_id: *session_id,
                })
                .unwrap_or(DirectoryClaim::Unclaimed);
            ReconciledDirectory {
                path: directory.path.clone(),
                claim,
                git_marker: directory.git_marker,
                estimated_bytes: directory.estimated_bytes,
            }
        })
        .collect()
}

/// Summarise a reconciliation for reporting.
pub fn summarise(
    reconciled: &[ReconciledDirectory],
    scanned_root_count: usize,
) -> WorktreeReconciliation {
    let unclaimed: Vec<&ReconciledDirectory> = reconciled
        .iter()
        .filter(|entry| entry.unclaimed())
        .collect();
    let sized = unclaimed
        .iter()
        .all(|entry| entry.estimated_bytes.is_some());
    WorktreeReconciliation {
        schema_version: WORKTREE_RECONCILIATION_SCHEMA_VERSION,
        scanned_root_count,
        directory_count: reconciled.len(),
        claimed_count: reconciled.len() - unclaimed.len(),
        unclaimed_count: unclaimed.len(),
        unclaimed_bytes: unclaimed
            .iter()
            .filter_map(|entry| entry.estimated_bytes)
            .fold(0_u64, |total, bytes| total.saturating_add(bytes)),
        sized,
        unclaimed: unclaimed
            .iter()
            .map(|entry| UnclaimedDirectory {
                path: entry.path.clone(),
                kind: entry.kind().to_string(),
                estimated_bytes: entry.estimated_bytes,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(path: &str, git_marker: bool, bytes: Option<u64>) -> ObservedDirectory {
        ObservedDirectory {
            path: path.to_string(),
            git_marker,
            estimated_bytes: bytes,
        }
    }

    #[test]
    fn a_directory_no_session_names_is_drift() {
        let reconciled = reconcile(
            &[observed("/root/alpha", true, Some(10))],
            &[(1, "/root/beta".into())],
        );
        assert!(reconciled[0].unclaimed());
    }

    #[test]
    fn a_directory_a_session_names_is_accounted_for() {
        let reconciled = reconcile(
            &[observed("/root/alpha", true, Some(10))],
            &[(7, "/root/alpha".into())],
        );
        assert_eq!(
            reconciled[0].claim,
            DirectoryClaim::Claimed { session_id: 7 }
        );
    }

    #[test]
    fn a_closed_sessions_worktree_is_not_drift() {
        // Closed sessions are passed in as claims precisely so this reports
        // ownership drift rather than re-reporting the cleanup backlog.
        let reconciled = reconcile(
            &[observed("/root/closed", true, Some(10))],
            &[(3, "/root/closed".into())],
        );
        assert!(!reconciled[0].unclaimed());
    }

    #[test]
    fn matching_is_exact_rather_than_prefix_based() {
        // "/root/alpha-2" must not be claimed by a session at "/root/alpha".
        let reconciled = reconcile(
            &[observed("/root/alpha-2", true, Some(10))],
            &[(1, "/root/alpha".into())],
        );
        assert!(
            reconciled[0].unclaimed(),
            "a prefix match would hide real drift behind a similarly named session"
        );
    }

    #[test]
    fn a_former_worktree_reads_differently_from_a_stray_directory() {
        let reconciled = reconcile(
            &[
                observed("/root/was-a-worktree", true, Some(10)),
                observed("/root/just-a-dir", false, Some(10)),
            ],
            &[],
        );
        assert_eq!(reconciled[0].kind(), "untracked_worktree");
        assert_eq!(reconciled[1].kind(), "stray_directory");
    }

    #[test]
    fn the_summary_counts_both_sides_and_totals_only_the_drift() {
        let reconciled = reconcile(
            &[
                observed("/root/claimed", true, Some(100)),
                observed("/root/drift-a", true, Some(10)),
                observed("/root/drift-b", false, Some(5)),
            ],
            &[(1, "/root/claimed".into())],
        );
        let summary = summarise(&reconciled, 1);
        assert_eq!(summary.directory_count, 3);
        assert_eq!(summary.claimed_count, 1);
        assert_eq!(summary.unclaimed_count, 2);
        assert_eq!(
            summary.unclaimed_bytes, 15,
            "the claimed directory's bytes belong to the cleanup lane, not to drift"
        );
        assert!(summary.sized);
    }

    #[test]
    fn an_unsized_sweep_says_so_rather_than_reporting_zero_bytes() {
        // Reporting `unclaimed_bytes: 0` for 16 unsized directories would read
        // as "nothing to reclaim", which is the opposite of the truth.
        let reconciled = reconcile(&[observed("/root/drift", true, None)], &[]);
        let summary = summarise(&reconciled, 1);
        assert_eq!(summary.unclaimed_count, 1);
        assert_eq!(summary.unclaimed_bytes, 0);
        assert!(!summary.sized, "callers must be able to tell these apart");
    }

    #[test]
    fn sizes_cannot_overflow_the_total() {
        let reconciled = reconcile(
            &[
                observed("/root/a", true, Some(u64::MAX)),
                observed("/root/b", true, Some(u64::MAX)),
            ],
            &[],
        );
        assert_eq!(summarise(&reconciled, 1).unclaimed_bytes, u64::MAX);
    }

    #[test]
    fn no_roots_scanned_is_distinguishable_from_an_empty_root() {
        assert_eq!(summarise(&[], 0).scanned_root_count, 0);
        assert_eq!(summarise(&[], 1).scanned_root_count, 1);
    }
}
