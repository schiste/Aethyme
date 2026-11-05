//! How far a session branch is from the fetched default branch.
//!
//! Since sessions deliver through their own pushed branches and pull requests
//! (#433), the default branch moves under a live session whenever another PR
//! merges. Overlap with *open* pull requests is reported elsewhere (#441); once
//! a PR merges its change stops being compared, so an agent editing the same
//! lines found out only when GitHub marked its own PR conflicting.
//!
//! This answers two questions without touching any worktree: how many commits
//! the branch is behind and ahead, and whether merging it with the default
//! branch would conflict (`git merge-tree --write-tree`). It is advisory: it
//! never blocks a push, a submit or a status.

use crate::git::{GitError, GitRepo};

/// `meta` key prefix for the last measured drift of one session.
const DRIFT_CACHE_PREFIX: &str = "main_drift.";

/// Sessions one `status` may re-measure when the cached verdict no longer
/// matches the current refs. Each measurement is three Git processes; the cap
/// keeps a status over many sessions from turning into many process spawns
/// (#455). Sessions beyond it show their row on a later status or after a push.
pub(crate) const STATUS_REMEASURE_BUDGET: usize = 3;

/// Conflicting paths a report names.
pub const CONFLICT_SAMPLE: usize = 5;

/// A session branch compared with the fetched default branch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DefaultBranchDrift {
    /// The remote-tracking ref compared against, e.g. `origin/main`.
    pub reference: String,
    /// Its commit at measurement time.
    pub commit: String,
    /// The session commit measured.
    pub head: String,
    /// Default-branch commits the session branch does not contain.
    pub behind: u64,
    /// Session commits the default branch does not contain.
    pub ahead: u64,
    /// Whether merging the two would conflict.
    pub would_conflict: bool,
    /// Up to [`CONFLICT_SAMPLE`] conflicting paths.
    pub conflicting_paths: Vec<String>,
    /// Revision-bound relationship assessment; unknown keeps the conservative sync advice.
    #[serde(default)]
    pub sync_impact: SyncImpact,
    /// How to catch up, when the branch is behind.
    pub suggested_command: Option<String>,
}

/// Whether the changed surfaces can safely defer an intermediate sync.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncImpact {
    #[default]
    Unknown,
    Related,
    Disjoint,
}

/// A disjoint verdict is valid only for clean, exactly indexed revisions on
/// both sides. Missing files, graph coverage or relationships are not proof.
pub(crate) fn assess_sync_impact(
    repo: &GitRepo,
    worktree: &std::path::Path,
    drift: &mut DefaultBranchDrift,
) {
    use crate::graph_impact::{
        GRAPH_IMPACT_MAX_BUDGET, GraphImpactContractStatus, GraphImpactMode,
        GraphStoreImpactProvider, revision_bound_impact_report,
    };
    use std::collections::BTreeSet;
    if drift.behind == 0 || drift.would_conflict {
        return;
    }
    let Ok(local) = GitRepo::discover(worktree) else {
        return;
    };
    if local.resolve_ref("HEAD").as_deref() != Some(drift.head.as_str())
        || !matches!(local.changed_files(&drift.head), Ok(files) if files.is_empty())
    {
        return;
    }
    let Ok(base) = repo.merge_base(&drift.head, &drift.commit) else {
        return;
    };
    let (Ok(ours), Ok(theirs)) = (
        repo.changed_between_all_paths(&base, &drift.head),
        repo.changed_between_all_paths(&base, &drift.commit),
    ) else {
        return;
    };
    if ours.is_empty() || theirs.is_empty() {
        return;
    }
    let ours_set: BTreeSet<_> = ours.iter().cloned().collect();
    if theirs.iter().any(|p| ours_set.contains(p)) {
        drift.sync_impact = SyncImpact::Related;
        return;
    }
    let mut surfaces = [BTreeSet::new(), BTreeSet::new()];
    for (root, revision) in [
        (local.root(), drift.head.as_str()),
        (repo.root(), drift.commit.as_str()),
    ] {
        for (index, files) in [&ours, &theirs].into_iter().enumerate() {
            for mode in [
                GraphImpactMode::Calls,
                GraphImpactMode::Imports,
                GraphImpactMode::Documents,
            ] {
                let Ok(report) = revision_bound_impact_report(
                    root,
                    revision,
                    revision,
                    files,
                    mode,
                    GRAPH_IMPACT_MAX_BUDGET,
                    &GraphStoreImpactProvider,
                ) else {
                    return;
                };
                if report.status != GraphImpactContractStatus::Complete
                    || report.coverage.unsupported_files > 0
                    || report.coverage.excluded_files > 0
                    || report.risk_hints.global_config_surface
                    || report.risk_hints.workspace_surface
                {
                    return;
                }
                let impact = report.impact;
                surfaces[index].extend(files.iter().cloned());
                for paths in [
                    impact.direct,
                    impact.transitive,
                    impact.callers,
                    impact.importers,
                    impact.documents,
                    impact.tests,
                    impact.configs,
                    impact.manifests,
                ] {
                    surfaces[index].extend(paths);
                }
            }
        }
    }
    if !surfaces[0].is_disjoint(&surfaces[1]) {
        drift.sync_impact = SyncImpact::Related;
        return;
    }
    drift.sync_impact = SyncImpact::Disjoint;
    drift.suggested_command = None;
}

/// How a branch catches up with the default branch.
///
/// A published branch is merged: rebasing it rewrites commits a pull request
/// already shows, which then needs a lease-guarded push. An unpublished branch
/// is rebased, keeping its history linear before anyone has seen it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatchUp {
    Merge,
    Rebase,
}

impl CatchUp {
    /// The deterministic choice for one branch: merge when it exists on the
    /// remote, rebase otherwise.
    pub(crate) fn for_branch(repo: &GitRepo, remote: &str, branch: &str) -> Self {
        if repo
            .resolve_ref(&format!("refs/remotes/{remote}/{branch}"))
            .is_some()
        {
            Self::Merge
        } else {
            Self::Rebase
        }
    }

    fn command(self, remote: &str, reference: &str) -> String {
        match self {
            Self::Merge => format!("git fetch {remote} && git merge {reference}"),
            Self::Rebase => format!("git fetch {remote} && git rebase {reference}"),
        }
    }
}

/// Compare `head` with the default branch at `commit`. Preserve Git failures
/// so refusing callers can explain why the comparison is unknown.
pub(crate) fn measure(
    repo: &GitRepo,
    reference: &str,
    commit: &str,
    head: &str,
    catch_up: CatchUp,
) -> Result<DefaultBranchDrift, GitError> {
    let behind = repo.commit_count_between(head, commit)?;
    let ahead = repo.commit_count_between(commit, head)?;
    // Nothing merged since the branch left the default branch: it cannot
    // conflict with it, so skip the simulation.
    let conflicts = if behind == 0 {
        Vec::new()
    } else {
        repo.merge_tree_simulate(commit, head)?.conflicts
    };
    let remote = reference
        .split_once('/')
        .map_or("origin", |(remote, _)| remote);
    Ok(DefaultBranchDrift {
        reference: reference.to_string(),
        commit: commit.to_string(),
        head: head.to_string(),
        behind,
        ahead,
        would_conflict: !conflicts.is_empty(),
        conflicting_paths: conflicts.into_iter().take(CONFLICT_SAMPLE).collect(),
        sync_impact: SyncImpact::Unknown,
        suggested_command: (behind > 0).then(|| catch_up.command(remote, reference)),
    })
}

pub(crate) fn cache_key(session_id: i64) -> String {
    format!("{DRIFT_CACHE_PREFIX}{session_id}")
}

/// A cached measurement, only when it was taken for exactly these refs.
pub(crate) fn cached(raw: Option<&str>, head: &str, commit: &str) -> Option<DefaultBranchDrift> {
    let drift: DefaultBranchDrift = serde_json::from_str(raw?).ok()?;
    (drift.head == head && drift.commit == commit).then_some(drift)
}

impl crate::Broker {
    /// One `session.behind-main` row per live session whose branch the
    /// default branch has moved past: info while it still merges cleanly, a
    /// warning when merging would conflict.
    ///
    /// No network: it compares with the last fetched default branch. A cached
    /// verdict is reused while both refs are unchanged; otherwise at most
    /// [`STATUS_REMEASURE_BUDGET`] sessions are re-measured per call.
    pub(crate) fn behind_main_advice(&self) -> Vec<crate::StatusAdvice> {
        let repo = self.repo_handle();
        let Some(default) = crate::session_push::tracked_default(repo) else {
            return Vec::new();
        };
        let Ok(sessions) = self.store_ref().live_sessions() else {
            return Vec::new();
        };
        let reference = format!("{}/{}", default.remote, default.branch);
        let mut budget = STATUS_REMEASURE_BUDGET;
        let mut advice = Vec::new();
        for session in sessions {
            // A session working on the default branch itself is the default
            // branch's local copy, not a branch that can fall behind it.
            if session.branch == default.branch {
                continue;
            }
            let Some(head) = repo.resolve_ref(&format!("refs/heads/{}", session.branch)) else {
                continue;
            };
            let key = cache_key(session.id);
            let raw = self.store_ref().meta_get(&key).ok().flatten();
            let mut drift = match cached(raw.as_deref(), &head, &default.commit) {
                Some(drift) => drift,
                None if budget > 0 => {
                    budget -= 1;
                    let catch_up = CatchUp::for_branch(repo, &default.remote, &session.branch);
                    let Ok(drift) = measure(repo, &reference, &default.commit, &head, catch_up)
                    else {
                        continue;
                    };
                    if let Ok(raw) = serde_json::to_string(&drift) {
                        crate::warn_unrecorded(
                            "cache the default-branch comparison",
                            self.store_ref().meta_set(&key, &raw),
                        );
                    }
                    drift
                }
                None => continue,
            };
            drift.sync_impact = SyncImpact::Unknown;
            if drift.behind > 0 {
                let catch_up = CatchUp::for_branch(repo, &default.remote, &session.branch);
                drift.suggested_command = Some(catch_up.command(&default.remote, &reference));
                assess_sync_impact(
                    repo,
                    std::path::Path::new(&session.worktree_path),
                    &mut drift,
                );
            }
            if let Some(row) = behind_main_row(session.id, &drift) {
                advice.push(row);
            }
        }
        advice
    }
}

fn behind_main_row(session_id: i64, drift: &DefaultBranchDrift) -> Option<crate::StatusAdvice> {
    if drift.behind == 0 {
        return None;
    }
    let (severity, reason, summary) = if drift.would_conflict {
        (
            crate::StatusAdviceSeverity::Warning,
            "the default branch moved and now conflicts with the session",
            format!(
                "session {session_id} is {} commit(s) behind {}, and merging it would conflict \
                 in {}; catch up before the pull request does",
                drift.behind,
                drift.reference,
                drift.conflicting_paths.join(", ")
            ),
        )
    } else if drift.sync_impact == SyncImpact::Disjoint {
        (
            crate::StatusAdviceSeverity::Info,
            "the changed surfaces are graph-disjoint",
            format!(
                "session {session_id} is {} commit(s) behind {}; graph impact is disjoint, so intermediate sync may be deferred; validate the current merge candidate before merging",
                drift.behind, drift.reference
            ),
        )
    } else {
        (
            crate::StatusAdviceSeverity::Info,
            "the default branch moved under the session",
            format!(
                "session {session_id} is {} commit(s) behind {}; it still merges cleanly",
                drift.behind, drift.reference
            ),
        )
    };
    Some(crate::StatusAdvice {
        id: "session.behind-main",
        severity,
        reason,
        summary,
        session_id: Some(session_id),
        queue_entry_id: None,
        evidence: vec![
            format!("head {}", &drift.head[..drift.head.len().min(12)]),
            format!(
                "{} {}",
                drift.reference,
                &drift.commit[..drift.commit.len().min(12)]
            ),
        ],
        commands: drift.suggested_command.iter().cloned().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drift(head: &str, commit: &str) -> DefaultBranchDrift {
        DefaultBranchDrift {
            reference: "origin/main".into(),
            commit: commit.into(),
            head: head.into(),
            behind: 2,
            ahead: 1,
            would_conflict: true,
            conflicting_paths: vec!["a.txt".into()],
            sync_impact: SyncImpact::Unknown,
            suggested_command: Some("git fetch origin && git merge origin/main".into()),
        }
    }

    fn graph(root: &std::path::Path, revision: &str, related: bool) {
        use aethyme_engine::model::{
            edge::{Edge, EdgeKind},
            file::{FileNode, FileRole},
        };
        use aethyme_engine::store::redb::graph_store::{
            GraphStore, RepoMetadata, insert_edge, insert_file,
        };
        use aethyme_graph_storage::{GraphCoverage, write_coverage_artifacts};
        let mut coverage = GraphCoverage::unavailable("fixture");
        coverage.available = true;
        coverage.source_revision = Some(revision.into());
        coverage.indexed_revision = Some(revision.into());
        coverage.source_tree_sha256 = Some("tree".into());
        coverage.indexed_tree_sha256 = Some("tree".into());
        coverage.coverage_mode = "complete".into();
        coverage.safe_to_use = true;
        coverage.gaps.clear();
        coverage.files.parsed = 2;
        coverage
            .by_language
            .insert("rust".into(), Default::default());
        for kind in ["calls", "imports", "documents", "tests"] {
            coverage.edge_counts_by_kind.insert(kind.into(), 0);
        }
        write_coverage_artifacts(root, &coverage, &[]).unwrap();
        let store = GraphStore::open(root).unwrap();
        let a = FileNode::new(
            "Repo",
            "a.rs",
            Some("rust".into()),
            FileRole::Source,
            1,
            1,
            false,
            None,
        );
        let b = FileNode::new(
            "Repo",
            "b.rs",
            Some("rust".into()),
            FileRole::Source,
            1,
            1,
            false,
            None,
        );
        let mut index = store.begin_index().unwrap();
        insert_file(&mut index, &a).unwrap();
        insert_file(&mut index, &b).unwrap();
        if related {
            insert_edge(
                &mut index,
                &Edge::new(&b.id, &a.id, EdgeKind::Imports, 1000, "fixture"),
            )
            .unwrap();
        }
        index.commit().unwrap();
        store
            .set_repo_metadata(&RepoMetadata {
                root_path: root.display().to_string(),
                commit_hash: Some(revision.into()),
                indexed_at_unix: 0,
                file_count: 2,
                languages: vec!["rust".into()],
            })
            .unwrap();
    }

    fn git(root: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    }

    #[test]
    fn complete_revision_bound_graphs_distinguish_disjoint_and_related_changes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), ".aethyme/\n").unwrap();
        for path in ["a.rs", "b.rs"] {
            std::fs::write(root.join(path), "base\n").unwrap();
        }
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "base"]);
        let worktree = temp.path().join("session");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-qb",
                "session",
                worktree.to_str().unwrap(),
            ],
        );
        std::fs::write(worktree.join("a.rs"), "ours\n").unwrap();
        git(&worktree, &["commit", "-qam", "ours"]);
        let head = git(&worktree, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("b.rs"), "theirs\n").unwrap();
        git(&root, &["commit", "-qam", "theirs"]);
        let commit = git(&root, &["rev-parse", "HEAD"]);
        graph(&root, &commit, false);
        graph(&worktree, &head, false);
        let repo = GitRepo::discover(&root).unwrap();
        let mut result = measure(&repo, "origin/main", &commit, &head, CatchUp::Merge).unwrap();
        assess_sync_impact(&repo, &worktree, &mut result);
        assert_eq!(result.sync_impact, SyncImpact::Disjoint);
        assert!(result.suggested_command.is_none());
        graph(&worktree, &head, true);
        let mut result = measure(&repo, "origin/main", &commit, &head, CatchUp::Merge).unwrap();
        assess_sync_impact(&repo, &worktree, &mut result);
        assert_eq!(result.sync_impact, SyncImpact::Related);
        assert!(result.suggested_command.is_some());
        std::fs::write(worktree.join("a.rs"), "dirty\n").unwrap();
        let mut result = measure(&repo, "origin/main", &commit, &head, CatchUp::Merge).unwrap();
        assess_sync_impact(&repo, &worktree, &mut result);
        assert_eq!(result.sync_impact, SyncImpact::Unknown);
    }

    #[test]
    fn a_cached_verdict_is_reused_only_for_the_same_refs() {
        let raw = serde_json::to_string(&drift("h1", "c1")).unwrap();
        assert!(cached(Some(&raw), "h1", "c1").is_some());
        assert!(cached(Some(&raw), "h2", "c1").is_none(), "session moved");
        assert!(
            cached(Some(&raw), "h1", "c2").is_none(),
            "default branch moved"
        );
        assert!(cached(None, "h1", "c1").is_none());
        assert!(cached(Some("not json"), "h1", "c1").is_none());
    }

    #[test]
    fn the_row_warns_on_conflict_informs_when_clean_and_is_silent_when_current() {
        let conflicting = behind_main_row(7, &drift("h1", "c1")).expect("row");
        assert_eq!(conflicting.severity, crate::StatusAdviceSeverity::Warning);
        assert!(
            conflicting.summary.contains("a.txt"),
            "{}",
            conflicting.summary
        );
        let clean = DefaultBranchDrift {
            would_conflict: false,
            conflicting_paths: Vec::new(),
            ..drift("h1", "c1")
        };
        assert_eq!(
            behind_main_row(7, &clean).expect("row").severity,
            crate::StatusAdviceSeverity::Info
        );
        let current = DefaultBranchDrift { behind: 0, ..clean };
        assert!(behind_main_row(7, &current).is_none());
    }

    #[test]
    fn a_published_branch_is_merged_and_an_unpublished_one_rebased() {
        assert_eq!(
            CatchUp::Merge.command("origin", "origin/main"),
            "git fetch origin && git merge origin/main"
        );
        assert_eq!(
            CatchUp::Rebase.command("origin", "origin/main"),
            "git fetch origin && git rebase origin/main"
        );
    }
}
