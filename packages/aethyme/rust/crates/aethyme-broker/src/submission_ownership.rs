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
//! Against one base the pending commits are the plan's, computed from fewer
//! commits. What is left out cannot change them: an inherited commit is never
//! owned, a commit the base reaches is integrated by ancestry whatever its
//! patch, and two commits that change different path sets never share a patch
//! identity. Where the plan fails on history it did not need, these answer.
//!
//! [`Broker::carried_ownership`] is that answer for integration, minus the
//! commits the remote default branch reaches.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

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
    /// Commits handed to hashing, counted before the attempt.
    hashed: usize,
}

impl Broker {
    /// The previous session's ownership boundary, when it still owns commits
    /// at `head` that were not delivered. `None` leaves the new session's
    /// baseline at `head`.
    ///
    /// The answer is the plan's against the integration view, minus the
    /// commits reachable from the fetched remote default branch, where pull
    /// requests land. Only reachability counts there: a patch match cannot say
    /// whether a commit is the one that landed or the same change applied
    /// again after a revert, so squash- and rebase-landed work stays carried.
    ///
    /// Leaving out an undelivered commit is not a display error. The new
    /// session's `diff_base`, `adoption_base` and `adopted_head` all become
    /// `head`, so submit replays nothing and the unsubmitted and unpushed
    /// guards of `finish` no longer see the work.
    ///
    /// `refs/remotes/origin/HEAD` names the default branch (the main
    /// checkout's `@{upstream}` can be a session's own pushed branch), and
    /// origin is trusted as the landing remote, as auto-cleanup trusts it.
    /// Without it, when it names the previous session's own branch, or when it
    /// cannot be read, the answer is integration's alone.
    pub(crate) fn carried_ownership(
        &self,
        previous: &Session,
        head: &str,
    ) -> Option<AdoptCarriedOwnership> {
        let _span = crate::phase_telemetry::span("adopt_carried_ownership");
        let mut work = PatchWork::default();
        let own_branch = format!("refs/remotes/origin/{}", previous.branch);
        let default_tip = self
            .remote_default_ref_and_tip()
            .filter(|(reference, _)| *reference != own_branch)
            .map(|(_, commit)| commit);
        let pending = self.undelivered_owned(previous, head, default_tip.as_deref(), &mut work);
        crate::phase_telemetry::attach(
            "adopt_ownership",
            serde_json::json!({ "carried_commits_hashed": work.hashed }),
        );
        if pending.is_err() {
            crate::phase_telemetry::add("adopt_carried_ownership_error", Duration::ZERO);
        }
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
        default_tip: Option<&str>,
        work: &mut PatchWork,
    ) -> Result<Option<(String, usize)>, BrokerOpError> {
        let repo = self.repo_handle();
        let (_, integration) = self.integration_head_snapshot()?;
        let checkpoint = OwnershipCheckpoint::select(repo, session, head);
        let Some(range) = OwnedRange::select(repo, &checkpoint, head, &integration) else {
            return Ok(None);
        };
        // A default branch that cannot be read proves nothing, and must not
        // cost the answer integration gives without it.
        let beyond_both = default_tip.and_then(|tip| {
            let outside = range.owned_outside(repo, head, &[&integration, tip]).ok();
            if outside.is_none() {
                crate::phase_telemetry::add("adopt_carried_default_unreadable", Duration::ZERO);
            }
            outside
        });
        let outside = match beyond_both {
            Some(outside) => outside,
            None => range.owned_outside(repo, head, &[&integration])?,
        };
        let pending = self
            .states_outside(&outside, &integration, range.baseline(), work)?
            .into_iter()
            .filter(|(_, state)| *state == SubmissionIntegrationState::Pending)
            .count();
        Ok(Some((range.baseline().to_string(), pending)))
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
        let matches = self.patch_match_counts(&compared, base, baseline, work)?;
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

    /// How many of `base`'s first-parent commits beyond `baseline` carry the
    /// same patch as each of `compared`; a commit without one is absent.
    fn patch_match_counts(
        &self,
        compared: &[(String, bool)],
        base: &str,
        baseline: &str,
        work: &mut PatchWork,
    ) -> Result<BTreeMap<String, usize>, BrokerOpError> {
        if compared.is_empty() {
            return Ok(BTreeMap::new());
        }
        let repo = self.repo_handle();
        let (candidates, comparable) = {
            let _span = crate::phase_telemetry::span("adopt_ownership_candidates");
            let candidates = repo.first_parent_commits_excluding_oldest(base, baseline)?;
            let shapes = repo.commit_shapes(&candidates)?;
            let candidates = candidates
                .into_iter()
                .map(|commit| {
                    let has_parent = !shapes[&commit].parents.is_empty();
                    (commit, has_parent)
                })
                .collect::<Vec<_>>();
            let comparable =
                repo.commits_sharing_changed_paths(compared, &candidates, &mut work.cache)?;
            (candidates, comparable)
        };
        work.hashed += repo.uncached_patch_id_count(&comparable, &work.cache);
        let patch_ids = repo.cached_first_parent_patch_ids(&comparable, &mut work.cache)?;
        let mut by_patch = BTreeMap::<&str, usize>::new();
        for (candidate, _) in &candidates {
            if let Some(patch) = patch_ids.get(candidate) {
                *by_patch.entry(patch).or_default() += 1;
            }
        }
        Ok(compared
            .iter()
            .filter_map(|(commit, _)| {
                let count = by_patch.get(patch_ids.get(commit)?.as_str())?;
                Some((commit.clone(), *count))
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
                let plan = self
                    .broker
                    .build_submission_plan(session, &self.head(), &self.base)
                    .ok();
                if let Some(plan) = plan {
                    assert_eq!(Some(&carried.baseline), plan.recorded_baseline.as_ref());
                }
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

    /// The default branch's copy of `commit`, as a squash merge leaves it.
    fn squash_land(fixture: &Fixture, commit: &str) -> String {
        git(&fixture.root, &["cherry-pick", "--no-commit", commit]);
        git(&fixture.root, &["commit", "-qm", "squash"]);
        git(&fixture.root, &["rev-parse", "HEAD"])
    }

    fn merge_land(fixture: &Fixture, commit: &str) -> String {
        git(
            &fixture.root,
            &["merge", "-q", "--no-ff", "-m", "land", commit],
        );
        git(&fixture.root, &["rev-parse", "HEAD"])
    }

    /// Landing A, then its revert R, then applying A again without syncing:
    /// the second A' is undelivered though its patch is on the default branch.
    #[test]
    fn a_change_applied_again_after_its_revert_landed_stays_owned() {
        for squash in [false, true] {
            let mut fixture = Fixture::new();
            fixture.set_integration(&fixture.base);
            let session = fixture.adopt();
            let land = if squash { squash_land } else { merge_land };
            let first = commit(&fixture.work, "owned.txt", "owned\n");
            fixture.set_upstream(&land(&fixture, &first));
            // By ancestry the landed commit is delivered; squashed, it stays.
            let landed = if squash { Some(1) } else { None };
            assert_eq!(fixture.carried(&session).0, landed, "squash: {squash}");

            git(&fixture.work, &["revert", "--no-edit", "HEAD"]);
            let revert = fixture.head();
            fixture.set_upstream(&land(&fixture, &revert));
            git(&fixture.work, &["revert", "--no-edit", "HEAD"]);
            let owned = if squash { Some(3) } else { Some(1) };
            assert_eq!(fixture.carried(&session).0, owned, "squash: {squash}");
        }
    }

    /// Someone else lands and reverts the very patch the session wrote.
    #[test]
    fn another_authors_landing_of_the_same_patch_delivers_nothing() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        let session = fixture.adopt();
        let owned = commit(&fixture.work, "flag.txt", "on\n");
        let upstream = squash_land(&fixture, &owned);
        fixture.set_upstream(&upstream);
        assert_eq!(fixture.carried(&session).0, Some(1));
        git(&fixture.root, &["revert", "--no-edit", "HEAD"]);
        fixture.set_upstream(&git(&fixture.root, &["rev-parse", "HEAD"]));
        assert_eq!(fixture.carried(&session).0, Some(1));
    }

    #[test]
    fn one_landing_does_not_deliver_a_patch_authored_twice() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        let session = fixture.adopt();
        let first = commit(&fixture.work, "owned.txt", "owned\n");
        git(&fixture.work, &["revert", "--no-edit", "HEAD"]);
        git(&fixture.work, &["cherry-pick", "-x", &first]);
        fixture.set_upstream(&merge_land(&fixture, &first));
        assert_eq!(fixture.carried(&session).0, Some(2));
    }

    /// The host reverts the landing; the session makes its own revert and
    /// applies the change again, both local.
    #[test]
    fn a_host_revert_and_a_local_reland_keep_the_local_commits_owned() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        let session = fixture.adopt();
        let first = commit(&fixture.work, "owned.txt", "owned\n");
        merge_land(&fixture, &first);
        git(&fixture.root, &["revert", "--no-edit", "-m", "1", "HEAD"]);
        fixture.set_upstream(&git(&fixture.root, &["rev-parse", "HEAD"]));
        git(&fixture.work, &["revert", "--no-edit", "HEAD"]);
        git(&fixture.work, &["revert", "--no-edit", "HEAD"]);
        assert_eq!(fixture.carried(&session).0, Some(2));

        // Rebased onto the host's revert instead, the reland is still owned.
        git(&fixture.work, &["reset", "-q", "--hard", UPSTREAM]);
        git(&fixture.work, &["revert", "--no-edit", "HEAD"]);
        assert_eq!(fixture.carried(&session).0, Some(1));
    }

    /// A rewritten baseline keeps the rebased commit after the default branch
    /// moved on, and merging that branch in adds only the merge itself.
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
        git(&fixture.work, &["merge", "-q", "--no-edit", "main"]);
        assert_eq!(fixture.carried(&session).0, Some(2));
    }

    #[test]
    fn a_default_branch_that_is_the_sessions_own_branch_proves_nothing() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        let session = fixture.adopt();
        let owned = commit(&fixture.work, "owned.txt", "owned\n");
        let own = format!("refs/remotes/origin/{}", session.branch);
        git(&fixture.root, &["update-ref", &own, &owned]);
        git(
            &fixture.root,
            &["symbolic-ref", "refs/remotes/origin/HEAD", &own],
        );
        assert_eq!(fixture.carried(&session).0, Some(1));
    }

    #[test]
    fn a_carry_that_fails_is_counted_apart_from_nothing_pending() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        fixture.set_upstream(&fixture.tip);
        let session = fixture.adopt();
        let errors = |head: &str| {
            let recorder = crate::phase_telemetry::Recorder::default();
            let carried = {
                let _installed = crate::phase_telemetry::install(recorder.clone());
                fixture.broker.carried_ownership(&session, head)
            };
            assert!(carried.is_none());
            recorder
                .totals()
                .1
                .get("adopt_carried_ownership_error")
                .copied()
        };
        assert_eq!(errors(&fixture.head()), None);
        // Without an integration ref the view is the main checkout's HEAD.
        git(&fixture.root, &["update-ref", "-d", INTEGRATION]);
        git(
            &fixture.root,
            &["symbolic-ref", "HEAD", "refs/heads/unborn"],
        );
        assert_eq!(errors(&fixture.head()), Some(1));
    }

    /// Delivered and undelivered work together: after adopting again, `finish`
    /// must still see the commit that is nowhere but here.
    #[test]
    fn finish_still_refuses_undelivered_work_after_adopting_again() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        let session = fixture.adopt();
        let landed = commit(&fixture.work, "landed.txt", "landed\n");
        commit(&fixture.work, "only-here.txt", "only here\n");
        fixture.set_upstream(&merge_land(&fixture, &landed));
        fixture.broker.close(session.id).unwrap();
        let again = fixture.adopt();
        assert_eq!(again.diff_base.as_deref(), Some(fixture.tip.as_str()));
        let report = fixture.broker.finish(again.id).unwrap();
        assert_eq!(report.unsubmitted_commits, 1, "{report:?}");
        assert!(report.pending_work.present, "{report:?}");
        assert!(!report.cleanup_safe, "{report:?}");
        assert!(fixture.work.is_dir());
    }

    /// The main checkout's `@{upstream}` is not where pull requests land.
    #[test]
    fn without_origin_head_the_main_checkouts_upstream_proves_nothing() {
        for local in [false, true] {
            let mut fixture = Fixture::new();
            fixture.set_integration(&fixture.base);
            let session = fixture.adopt();
            let owned = commit(&fixture.work, "owned.txt", "owned\n");
            let (remote, merge) = if local {
                (".", "refs/heads/work")
            } else {
                git(
                    &fixture.root,
                    &["update-ref", "refs/remotes/origin/work", &owned],
                );
                git(&fixture.root, &["config", "remote.origin.url", "."]);
                git(
                    &fixture.root,
                    &[
                        "config",
                        "remote.origin.fetch",
                        "+refs/heads/*:refs/remotes/origin/*",
                    ],
                );
                ("origin", "refs/heads/work")
            };
            git(&fixture.root, &["config", "branch.main.remote", remote]);
            git(&fixture.root, &["config", "branch.main.merge", merge]);
            assert_eq!(
                git(&fixture.root, &["rev-parse", "@{upstream}"]),
                owned,
                "the misleading upstream resolves"
            );
            assert_eq!(fixture.carried(&session).0, Some(1), "local: {local}");
        }
    }

    #[test]
    fn an_unreadable_default_branch_leaves_the_integration_answer() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        let session = fixture.adopt();
        commit(&fixture.work, "owned.txt", "owned\n");
        let upstream = commit(&fixture.root, "other.txt", "other\n");
        fixture.set_upstream(&upstream);
        git(
            &fixture.root,
            &["update-ref", "refs/heads/main", &fixture.tip],
        );
        std::fs::remove_file(
            fixture
                .root
                .join(".git/objects")
                .join(&upstream[..2])
                .join(&upstream[2..]),
        )
        .unwrap();
        assert_eq!(fixture.carried(&session).0, Some(1), "the tip is gone");

        // A tip that resolved but cannot be walked fails the listing itself.
        let head = fixture.head();
        assert!(
            fixture
                .broker
                .repo_handle()
                .commits_excluding_all_oldest(&head, &[&fixture.base, &upstream])
                .is_err()
        );
        let (_, pending) = fixture
            .broker
            .undelivered_owned(&session, &head, Some(&upstream), &mut PatchWork::default())
            .unwrap()
            .unwrap();
        assert_eq!(pending, 1);
    }

    #[test]
    fn accepted_and_repaired_checkpoints_start_ownership_where_the_plan_does() {
        let mut fixture = Fixture::new();
        fixture.set_integration(&fixture.base);
        let adopted = fixture.adopt();
        let accepted = commit(&fixture.work, "accepted.txt", "accepted\n");
        commit(&fixture.work, "later.txt", "later\n");
        fixture.set_upstream(&fixture.tip);

        let mut session = adopted.clone();
        session.accepted_session_head = Some(accepted.clone());
        assert_eq!(fixture.planned(&session, &fixture.base), 1);
        assert_eq!(fixture.carried(&session).0, Some(1));

        // The accepted checkpoint was rewritten and the broker recorded a
        // repair baseline; without one, ownership is ambiguous.
        git(
            &fixture.root,
            &["checkout", "-q", "--detach", &fixture.base],
        );
        let rewritten = commit(&fixture.root, "gone.txt", "gone\n");
        git(&fixture.root, &["checkout", "-q", "main"]);
        session.accepted_session_head = Some(rewritten);
        assert_eq!(fixture.planned(&session, &fixture.base), 0);
        assert_eq!(fixture.carried(&session).0, None);
        session.diff_base = Some(accepted);
        assert_eq!(fixture.planned(&session, &fixture.base), 1);
        assert_eq!(fixture.carried(&session).0, Some(1));
    }

    #[test]
    fn mode_only_merge_and_empty_commits_answer_as_the_plan_does() {
        let mut fixture = Fixture::new();
        let session = fixture.adopt();
        git(&fixture.work, &["update-index", "--chmod=+x", "base.txt"]);
        git(&fixture.work, &["commit", "-qm", "mode"]);
        git(&fixture.work, &["checkout", "-q", "--", "base.txt"]);
        let mode = fixture.head();
        git(
            &fixture.work,
            &["commit", "-q", "--allow-empty", "-m", "empty"],
        );
        git(&fixture.work, &["checkout", "-q", "-b", "side"]);
        commit(&fixture.work, "side.txt", "side\n");
        git(&fixture.work, &["checkout", "-q", "work"]);
        git(
            &fixture.work,
            &["merge", "-q", "--no-ff", "-m", "merge", "side"],
        );
        let merge = fixture.head();

        fixture.set_integration(&fixture.base);
        assert_eq!(fixture.planned(&session, &fixture.base), 4);
        assert_eq!(fixture.carried(&session).0, Some(4));

        fixture.land(&mode);
        git(&fixture.root, &["cherry-pick", "-x", "-m", "1", &merge]);
        let landed = git(&fixture.root, &["rev-parse", "HEAD"]);
        // Landings on the default branch by patch prove nothing.
        fixture.set_upstream(&landed);
        assert_eq!(fixture.carried(&session).0, Some(4));
        // On integration the plan decides: the side commit has the merge's
        // patch, so only the empty commit is left.
        assert_eq!(fixture.planned(&session, &landed), 1);
        fixture.set_integration(&landed);
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
