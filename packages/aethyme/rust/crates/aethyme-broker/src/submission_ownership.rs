//! Which session-owned commits a base does not have yet, without building a
//! submission plan.
//!
//! [`Broker::build_submission_plan`] reports every commit a session HEAD has
//! beyond a base, inherited history included, with its patch identity and
//! every match. Adoption needs far less, and needs it while it holds the
//! repository lifecycle claim: whether any owned commit is still pending. A
//! branch cut from a default branch far ahead of a local integration ref that
//! nothing moves carried all of that drift through one patch stream there.
//!
//! The answers here are the plan's, computed from fewer commits. What is left
//! out cannot change them: an inherited commit is never owned, a commit the
//! base reaches is integrated by ancestry whatever its patch, and two commits
//! that change different path sets never share a patch identity.

use std::collections::{BTreeMap, BTreeSet};

use crate::broker::AdoptCarriedOwnership;
use crate::git::{GitRepo, PatchIdCache};
use crate::merge::SubmissionIntegrationState;
use crate::types::Session;
use crate::{Broker, BrokerOpError};

/// The commit a session's ownership starts after, and why.
pub(crate) struct OwnershipCheckpoint<'a> {
    pub(crate) accepted: Option<&'a str>,
    /// Broker-recorded baseline that replaced a rewritten accepted checkpoint.
    pub(crate) repaired: Option<&'a str>,
    pub(crate) selected: Option<&'a str>,
}

impl<'a> OwnershipCheckpoint<'a> {
    pub(crate) fn select(repo: &GitRepo, session: &'a Session, session_head: &str) -> Self {
        // Once a contribution has been promoted, ownership begins after the
        // last accepted session HEAD. `diff_base` remains the operational
        // fallback for a session that has never contributed.
        let accepted = session.accepted_session_head.as_deref();
        let accepted_is_ancestor =
            accepted.is_some_and(|checkpoint| repo.is_ancestor(checkpoint, session_head));
        let repaired = session.diff_base.as_deref().filter(|checkpoint| {
            accepted.is_some()
                && !accepted_is_ancestor
                && session.adoption_base.as_deref() != Some(*checkpoint)
                && repo.is_ancestor(checkpoint, session_head)
        });
        let selected = if accepted_is_ancestor {
            accepted
        } else {
            repaired.or_else(|| {
                if accepted.is_none() {
                    session.diff_base.as_deref()
                } else {
                    accepted
                }
            })
        };
        Self {
            accepted,
            repaired,
            selected,
        }
    }
}

/// Where owned commits start, as [`Broker::build_submission_plan`] decides it.
enum OwnedRange<'a> {
    /// The recorded baseline is in the session's history.
    AfterBaseline(&'a str),
    /// The baseline was rewritten and the session is rebased onto the base:
    /// everything beyond the base is owned.
    BeyondBase(&'a str),
}

impl<'a> OwnedRange<'a> {
    /// `None` when ownership is ambiguous, where the plan reports no pending
    /// owned commit.
    fn select(
        repo: &GitRepo,
        checkpoint: &OwnershipCheckpoint<'a>,
        session_head: &str,
        base: &str,
    ) -> Option<Self> {
        let baseline = checkpoint.selected?;
        if repo.is_ancestor(baseline, session_head) {
            Some(Self::AfterBaseline(baseline))
        } else if checkpoint.accepted.is_none() && repo.is_ancestor(base, session_head) {
            Some(Self::BeyondBase(baseline))
        } else {
            None
        }
    }

    fn baseline(&self) -> &'a str {
        match self {
            Self::AfterBaseline(baseline) | Self::BeyondBase(baseline) => baseline,
        }
    }

    /// Owned commits none of `bases` reaches, oldest first.
    fn owned_outside(
        &self,
        repo: &GitRepo,
        session_head: &str,
        bases: &[&str],
    ) -> Result<Vec<String>, BrokerOpError> {
        let mut excluded = bases.to_vec();
        if let Self::AfterBaseline(baseline) = self {
            excluded.push(baseline);
        }
        Ok(repo.commits_excluding_all_oldest(session_head, &excluded)?)
    }
}

/// Patch identities computed for one answer, shared by its comparisons.
#[derive(Default)]
struct PatchWork {
    cache: PatchIdCache,
    hashed: usize,
}

impl Broker {
    /// The previous session's ownership boundary, when it still owns commits
    /// at `head` that were not delivered. `None` leaves the new session's
    /// baseline at `head`.
    ///
    /// A commit is undelivered when the integration view holds it pending and
    /// the fetched default branch, where pull requests land, does not prove it
    /// delivered. So this is the integration answer minus proven deliveries:
    /// work promoted into integration but not yet on the default branch stays
    /// delivered, and only positive evidence on the default branch (ancestry,
    /// an exact sync merge, or one patch-equivalent commit) can take a commit
    /// out of the count. An ambiguous match there or a comparison that fails
    /// proves nothing and leaves the commit owned.
    pub(crate) fn carried_ownership(
        &self,
        previous: &Session,
        head: &str,
    ) -> Option<AdoptCarriedOwnership> {
        let _span = crate::phase_telemetry::span("adopt_carried_ownership");
        let mut work = PatchWork::default();
        let pending = self.undelivered_owned(previous, head, &mut work);
        crate::phase_telemetry::attach(
            "adopt_ownership",
            serde_json::json!({ "carried_commits_hashed": work.hashed }),
        );
        let (baseline, pending_owned_commits) = pending.ok()?.filter(|(_, count)| *count > 0)?;
        Some(AdoptCarriedOwnership {
            from_session: previous.id,
            baseline,
            pending_owned_commits,
        })
    }

    fn undelivered_owned(
        &self,
        session: &Session,
        head: &str,
        work: &mut PatchWork,
    ) -> Result<Option<(String, usize)>, BrokerOpError> {
        let repo = self.repo_handle();
        let (_, integration) = self.integration_head_snapshot()?;
        let upstream = self
            .upstream_verification_head()
            .map(|(_, commit)| commit)
            .filter(|commit| *commit != integration);
        let checkpoint = OwnershipCheckpoint::select(repo, session, head);
        let Some(range) = OwnedRange::select(repo, &checkpoint, head, &integration) else {
            return Ok(None);
        };
        let baseline = range.baseline();
        let mut bases = vec![integration.as_str()];
        bases.extend(upstream.as_deref());
        let outside = range.owned_outside(repo, head, &bases)?;
        let mut pending = self
            .states_outside(&outside, &integration, baseline, work)?
            .into_iter()
            .filter(|(_, state)| *state == SubmissionIntegrationState::Pending)
            .map(|(commit, _)| commit)
            .collect::<Vec<_>>();
        if let Some(upstream) = &upstream
            && let Ok(states) = self.states_outside(&pending, upstream, baseline, work)
        {
            pending = states
                .into_iter()
                .filter(|(_, state)| {
                    matches!(
                        state,
                        SubmissionIntegrationState::Pending | SubmissionIntegrationState::Ambiguous
                    )
                })
                .map(|(commit, _)| commit)
                .collect();
        }
        Ok(Some((baseline.to_string(), pending.len())))
    }

    /// How many owned commits a submission against `base` would replay, when
    /// `base` is an ancestor of `session_head`: the plan's pending owned
    /// count, or 0 when the plan would be unsafe or cannot be judged.
    pub(crate) fn submittable_owned_commits(
        &self,
        session: &Session,
        session_head: &str,
        base: &str,
    ) -> usize {
        let mut work = PatchWork::default();
        let count = self
            .submittable_owned(session, session_head, base, &mut work)
            .unwrap_or(0);
        crate::phase_telemetry::attach(
            "adopt_ownership",
            serde_json::json!({ "drift_commits_hashed": work.hashed }),
        );
        count
    }

    fn submittable_owned(
        &self,
        session: &Session,
        session_head: &str,
        base: &str,
        work: &mut PatchWork,
    ) -> Result<usize, BrokerOpError> {
        let repo = self.repo_handle();
        let checkpoint = OwnershipCheckpoint::select(repo, session, session_head);
        let Some(range) = OwnedRange::select(repo, &checkpoint, session_head, base) else {
            return Ok(0);
        };
        let owned = range.owned_outside(repo, session_head, &[base])?;
        if owned.is_empty() {
            return Ok(0);
        }
        // The plan is safe only when no commit in it is ambiguous, and a plan
        // without an accepted checkpoint includes the inherited commits.
        let judged = if checkpoint.accepted.is_some() {
            owned.clone()
        } else {
            repo.commits_excluding_oldest(session_head, base)?
        };
        let states = self.states_outside(&judged, base, range.baseline(), work)?;
        if states
            .iter()
            .any(|(_, state)| *state == SubmissionIntegrationState::Ambiguous)
        {
            return Ok(0);
        }
        let owned = owned.into_iter().collect::<BTreeSet<_>>();
        Ok(states
            .iter()
            .filter(|(commit, state)| {
                *state == SubmissionIntegrationState::Pending && owned.contains(commit)
            })
            .count())
    }

    /// The integration state a plan against `base` gives each of `commits`,
    /// in order. Every commit must be one `base` does not reach.
    fn states_outside(
        &self,
        commits: &[String],
        base: &str,
        baseline: &str,
        work: &mut PatchWork,
    ) -> Result<Vec<(String, SubmissionIntegrationState)>, BrokerOpError> {
        if commits.is_empty() {
            return Ok(Vec::new());
        }
        let repo = self.repo_handle();
        let shapes = repo.commit_shapes(commits)?;
        let outside = commits.iter().collect::<BTreeSet<_>>();
        let second_parents = shapes
            .values()
            .filter(|shape| shape.parents.len() == 2 && !outside.contains(&shape.parents[1]))
            .map(|shape| shape.parents[1].clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let second_parent_shapes = repo.commit_shapes(&second_parents)?;
        let exact_sync = |commit: &String| {
            let shape = &shapes[commit];
            shape.parents.len() == 2
                && second_parent_shapes
                    .get(&shape.parents[1])
                    .is_some_and(|second| second.tree == shape.tree)
                && repo.is_ancestor(&shape.parents[1], base)
        };
        let synced = commits
            .iter()
            .filter(|commit| exact_sync(commit))
            .collect::<BTreeSet<_>>();
        let compared = commits
            .iter()
            .filter(|commit| !synced.contains(commit) && !shapes[*commit].parents.is_empty())
            .map(|commit| (commit.clone(), true))
            .collect::<Vec<_>>();
        let matches = if compared.is_empty() {
            BTreeMap::new()
        } else {
            let candidates = repo.first_parent_commits_excluding_oldest(base, baseline)?;
            let candidate_shapes = repo.commit_shapes(&candidates)?;
            let candidates = candidates
                .into_iter()
                .map(|commit| {
                    let has_parent = !candidate_shapes[&commit].parents.is_empty();
                    (commit, has_parent)
                })
                .collect::<Vec<_>>();
            let (matches, hashed) =
                repo.patch_equivalent_counts(&compared, &candidates, &mut work.cache)?;
            work.hashed += hashed;
            matches
        };
        Ok(commits
            .iter()
            .map(|commit| {
                let state = if synced.contains(commit) {
                    SubmissionIntegrationState::AlreadyIntegratedByAncestry
                } else {
                    match matches.get(commit).copied().unwrap_or(0) {
                        0 => SubmissionIntegrationState::Pending,
                        1 => SubmissionIntegrationState::AlreadyIntegratedByStablePatchIdentity,
                        _ => SubmissionIntegrationState::Ambiguous,
                    }
                };
                (commit.clone(), state)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::AdoptMode;
    use crate::test_storage::TestStorage;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.test")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn commit(dir: &Path, name: &str, content: &str) -> String {
        std::fs::write(dir.join(name), content).unwrap();
        git(dir, &["add", name]);
        git(dir, &["commit", "-q", "-m", content]);
        git(dir, &["rev-parse", "HEAD"])
    }

    const INTEGRATION: &str = "refs/heads/aethyme/integration";
    const UPSTREAM: &str = "refs/remotes/origin/main";

    /// `main` is `DRIFT` commits past `base`; `work` is a worktree cut from
    /// its tip. No integration or default-branch ref exists until a test sets
    /// one.
    struct Fixture {
        _repository: tempfile::TempDir,
        _storage: TestStorage,
        broker: Broker,
        root: PathBuf,
        work: PathBuf,
        base: String,
        tip: String,
    }

    const DRIFT: usize = 6;

    impl Fixture {
        fn new() -> Self {
            let repository = tempfile::tempdir().unwrap();
            let root = repository.path().join("repository");
            std::fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let storage = TestStorage::new();
            std::fs::create_dir_all(storage.worktree_root()).unwrap();
            git(&root, &["init", "-q", "-b", "main"]);
            git(&root, &["config", "gc.auto", "0"]);
            let base = commit(&root, "base.txt", "base\n");
            for step in 0..DRIFT {
                commit(&root, "drift.txt", &format!("drift {step}\n"));
            }
            let tip = git(&root, &["rev-parse", "HEAD"]);
            let work = storage.worktree_root().join("work");
            git(
                &root,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "work",
                    work.to_str().unwrap(),
                ],
            );
            let broker = storage.open(&root).unwrap();
            Self {
                _repository: repository,
                _storage: storage,
                broker,
                root,
                work,
                base,
                tip,
            }
        }

        fn set_integration(&self, commit: &str) {
            git(&self.root, &["update-ref", INTEGRATION, commit]);
        }

        fn set_upstream(&self, commit: &str) {
            git(&self.root, &["update-ref", UPSTREAM, commit]);
            git(
                &self.root,
                &["symbolic-ref", "refs/remotes/origin/HEAD", UPSTREAM],
            );
        }

        fn adopt(&mut self) -> Session {
            let work = self.work.clone();
            self.broker.adopt(&work, Some("carried work")).unwrap()
        }

        fn head(&self) -> String {
            git(&self.work, &["rev-parse", "HEAD"])
        }

        /// The pending count the carry reports, and how many commits it hashed.
        fn carried(&self, session: &Session) -> (Option<usize>, u64) {
            let recorder = crate::phase_telemetry::Recorder::default();
            let carried = {
                let _installed = crate::phase_telemetry::install(recorder.clone());
                self.broker.carried_ownership(session, &self.head())
            };
            assert_eq!(recorder.totals().1["adopt_carried_ownership"], 1);
            if let Some(carried) = &carried {
                assert_eq!(Some(&carried.baseline), session.diff_base.as_ref());
                assert_eq!(carried.from_session, session.id);
            }
            (
                carried.map(|carried| carried.pending_owned_commits),
                recorder.attachments()["adopt_ownership"]["carried_commits_hashed"]
                    .as_u64()
                    .unwrap(),
            )
        }

        fn planned(&self, session: &Session, base: &str) -> usize {
            self.broker
                .build_submission_plan(session, &self.head(), base)
                .unwrap()
                .pending_owned_commit_ids()
                .len()
        }

        /// Land `commit` on `main` under a new SHA.
        fn land(&self, commit: &str) -> String {
            git(&self.root, &["cherry-pick", "-x", commit]);
            git(&self.root, &["rev-parse", "HEAD"])
        }

        fn land_twice(&self, commit: &str) -> String {
            self.land(commit);
            git(&self.root, &["revert", "--no-edit", "HEAD"]);
            self.land(commit)
        }
    }

    #[test]
    fn a_stale_integration_ref_does_not_make_adoption_render_inherited_history() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        fixture.set_upstream(&fixture.tip);
        let session = fixture.adopt();
        commit(&fixture.work, "one.txt", "one\n");
        commit(&fixture.work, "two.txt", "two\n");
        assert_eq!(
            fixture.planned(&session, &fixture.base),
            2,
            "the plan's answer"
        );

        // Rendering any inherited patch needs a blob that is now missing.
        let blob = git(&fixture.root, &["rev-parse", "main~2:drift.txt"]);
        let object = fixture
            .root
            .join(".git/objects")
            .join(&blob[..2])
            .join(&blob[2..]);
        let saved = std::fs::read(&object).unwrap();
        std::fs::remove_file(&object).unwrap();
        assert!(
            fixture
                .broker
                .build_submission_plan(&session, &fixture.head(), &fixture.base)
                .is_err(),
            "the full plan renders inherited history"
        );
        assert_eq!(fixture.carried(&session), (Some(2), 0));
        std::fs::write(&object, saved).unwrap();

        // The whole adoption carries the boundary and reports the commits.
        fixture.broker.close(session.id).unwrap();
        let work = fixture.work.clone();
        let recorder = crate::phase_telemetry::Recorder::default();
        let report = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            fixture
                .broker
                .adopt_with(&work, Some("after close"), AdoptMode::New, None)
                .unwrap()
        };
        assert_eq!(report.carried_ownership.unwrap().pending_owned_commits, 2);
        assert_eq!(
            report.session.diff_base.as_deref(),
            Some(fixture.tip.as_str())
        );
        let warning = report.integration_drift.unwrap().warning.unwrap();
        assert!(
            warning.contains("2 pending session-owned commit(s) are safe to submit"),
            "{warning}"
        );
        assert_eq!(recorder.totals().1["adopt_drift_report"], 1);
        assert_eq!(
            recorder.attachments()["adopt_ownership"],
            serde_json::json!({ "carried_commits_hashed": 0, "drift_commits_hashed": 0 })
        );
    }

    /// Commits promoted into integration but absent from the default branch
    /// stay delivered; a later commit stays owned.
    #[test]
    fn promoted_commits_the_default_branch_lacks_do_not_become_pending() {
        for by_ancestry in [true, false] {
            let mut fixture = Fixture::new();
            fixture.set_upstream(&fixture.tip);
            let session = fixture.adopt();
            let promoted = commit(&fixture.work, "promoted.txt", "promoted\n");
            let integration = if by_ancestry {
                promoted
            } else {
                fixture.land(&promoted)
            };
            fixture.set_integration(&integration);
            assert_eq!(fixture.carried(&session).0, None, "{by_ancestry}");
            commit(&fixture.work, "later.txt", "later\n");
            assert_eq!(fixture.planned(&session, &integration), 1);
            assert_eq!(fixture.carried(&session).0, Some(1), "{by_ancestry}");
        }
    }

    #[test]
    fn a_patch_landed_on_the_default_branch_is_not_pending() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        let session = fixture.adopt();
        let landed = commit(&fixture.work, "landed.txt", "landed\n");
        let upstream = fixture.land(&landed);
        fixture.set_upstream(&upstream);
        assert_eq!(
            fixture.carried(&session),
            (None, 2),
            "one commit and its carrier"
        );
        commit(&fixture.work, "later.txt", "later\n");
        assert_eq!(fixture.carried(&session), (Some(1), 2));
        assert_eq!(fixture.planned(&session, &upstream), 1);
    }

    #[test]
    fn an_ambiguous_match_never_proves_delivery_on_the_default_branch() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        let session = fixture.adopt();
        let owned = commit(&fixture.work, "owned.txt", "owned\n");
        let upstream = fixture.land_twice(&owned);
        fixture.set_upstream(&upstream);
        assert_eq!(fixture.planned(&session, &fixture.base), 1);
        assert_eq!(fixture.carried(&session).0, Some(1));
    }

    #[test]
    fn a_failed_default_branch_comparison_leaves_commits_owned() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        let session = fixture.adopt();
        let owned = commit(&fixture.work, "owned.txt", "owned\n");
        let upstream = fixture.land(&owned);
        fixture.set_upstream(&upstream);
        let blob = git(&fixture.root, &["rev-parse", "main:owned.txt"]);
        std::fs::remove_file(
            fixture
                .root
                .join(".git/objects")
                .join(&blob[..2])
                .join(&blob[2..]),
        )
        .unwrap();
        assert_eq!(fixture.carried(&session).0, Some(1));
    }

    /// Without a default branch the answer is exactly the plan's.
    #[test]
    fn without_a_default_branch_the_answer_is_the_plans() {
        for landing in ["none", "once", "twice"] {
            let mut fixture = Fixture::new();
            let session = fixture.adopt();
            let owned = commit(&fixture.work, "owned.txt", "owned\n");
            let integration = match landing {
                "none" => fixture.base.clone(),
                "once" => fixture.land(&owned),
                _ => fixture.land_twice(&owned),
            };
            fixture.set_integration(&integration);
            let planned = fixture.planned(&session, &integration);
            assert_eq!(planned, usize::from(landing == "none"), "{landing}");
            assert_eq!(
                fixture.carried(&session).0,
                Some(planned).filter(|count| *count > 0),
                "{landing}"
            );
        }
    }

    /// A rebased session keeps its commits even after the default branch
    /// moved past what it was rebased onto, and an upstream merge into the
    /// session counts none of the merged commits.
    #[test]
    fn rewritten_and_synced_sessions_keep_only_their_own_commits() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        commit(&fixture.work, "before.txt", "before\n");
        let session = fixture.adopt();
        git(&fixture.work, &["reset", "-q", "--hard", &fixture.tip]);
        commit(&fixture.work, "rebased.txt", "rebased\n");
        let moved = commit(&fixture.root, "moved.txt", "moved\n");
        fixture.set_upstream(&moved);
        assert_eq!(fixture.carried(&session).0, Some(1));

        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        let session = fixture.adopt();
        let moved = commit(&fixture.root, "moved.txt", "moved\n");
        fixture.set_upstream(&moved);
        // A no-fast-forward merge that adds nothing is an exact sync.
        git(
            &fixture.work,
            &["merge", "-q", "--no-ff", "-m", "sync", "main"],
        );
        assert_eq!(fixture.carried(&session).0, None);
        commit(&fixture.work, "mine.txt", "mine\n");
        assert_eq!(fixture.carried(&session).0, Some(1));
    }

    #[test]
    fn the_drift_report_counts_what_the_plan_would_submit() {
        for (case, expected) in [("owned", 1), ("inherited", 0), ("reapplied", 0)] {
            let mut fixture = Fixture::new();
            fixture.set_upstream(&fixture.tip);
            if case == "inherited" {
                commit(&fixture.work, "inherited.txt", "inherited\n");
            }
            let session = fixture.adopt();
            match case {
                "owned" => {
                    commit(&fixture.work, "owned.txt", "owned\n");
                }
                // The default branch reverted the session's commit twice
                // over; applying it again is ambiguous, so unsafe.
                "reapplied" => {
                    let owned = commit(&fixture.work, "owned.txt", "owned\n");
                    let upstream = fixture.land_twice(&owned);
                    git(&fixture.root, &["revert", "--no-edit", "HEAD"]);
                    let reverted = git(&fixture.root, &["rev-parse", "HEAD"]);
                    assert_ne!(upstream, reverted);
                    fixture.set_upstream(&reverted);
                    git(&fixture.work, &["reset", "-q", "--hard", &reverted]);
                    git(&fixture.work, &["cherry-pick", "-x", &owned]);
                    commit(&fixture.work, "more.txt", "more\n");
                }
                _ => {}
            }
            let base = git(&fixture.root, &["rev-parse", UPSTREAM]);
            let head = fixture.head();
            let plan = fixture
                .broker
                .build_submission_plan(&session, &head, &base)
                .unwrap();
            let planned = if plan.safe {
                plan.pending_owned_commit_ids().len()
            } else {
                0
            };
            assert_eq!(planned, expected, "{case}");
            assert_eq!(
                fixture
                    .broker
                    .submittable_owned_commits(&session, &head, &base),
                planned,
                "{case}"
            );
        }
    }
}
