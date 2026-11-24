use super::*;

use std::process::Command;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn branch_exists(root: &Path, name: &str) -> bool {
    Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ])
        .current_dir(root)
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn the_rule_deletes_only_merged_unused_branches_of_cleaned_sessions() {
    let base = BranchFacts {
        session_not_cleaned: false,
        checked_out: false,
        recovery_archive: false,
        merged: Some(true),
    };
    assert_eq!(judge(base), Verdict::Delete);
    assert_eq!(
        judge(BranchFacts {
            session_not_cleaned: true,
            ..base
        }),
        Verdict::Keep("session_not_cleaned")
    );
    assert_eq!(
        judge(BranchFacts {
            checked_out: true,
            ..base
        }),
        Verdict::Keep("checked_out")
    );
    assert_eq!(
        judge(BranchFacts {
            recovery_archive: true,
            ..base
        }),
        Verdict::Keep("recovery_archive")
    );
    assert_eq!(
        judge(BranchFacts {
            merged: Some(false),
            ..base
        }),
        Verdict::Keep("unmerged")
    );
    assert_eq!(
        judge(BranchFacts {
            merged: None,
            ..base
        }),
        Verdict::Keep("unknown")
    );
}

struct Fixture {
    _owner: tempfile::TempDir,
    _storage: crate::test_storage::TestStorage,
    root: std::path::PathBuf,
    broker: crate::Broker,
}

fn fixture() -> Fixture {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("repository");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    git(&root, &["add", ".gitignore", "base.txt"]);
    git(&root, &["commit", "-qm", "fixture"]);
    aethyme_testkit::ensure_origin(&root);
    git(&root, &["remote", "set-head", "origin", "main"]);
    let storage = crate::test_storage::TestStorage::new();
    let broker = storage.open(&root).unwrap();
    Fixture {
        _owner: owner,
        _storage: storage,
        root,
        broker,
    }
}

impl Fixture {
    /// A session on `branch`, cleaned unless `open`.
    fn session(&mut self, branch: &str, open: bool) -> i64 {
        let worktree = self.root.with_file_name(branch.replace('/', "-"));
        let session = self
            .broker
            .store()
            .register_session(&crate::NewSession {
                worktree_path: worktree.to_string_lossy().into_owned(),
                branch: branch.into(),
                origin: crate::SessionOrigin::Adopted,
                task: Some(format!("work on {branch}")),
                diff_base: None,
                adoption_base: None,
                adopted_head: None,
                repository_contract: None,
                pid: None,
                command: None,
                log_path: None,
                agent_identity: None,
            })
            .unwrap();
        if !open {
            self.broker
                .store()
                .set_session_status(session.id, crate::SessionStatus::Cleaned, None)
                .unwrap();
        }
        session.id
    }
}

#[test]
fn a_slice_deletes_merged_branches_of_cleaned_sessions_and_keeps_the_rest() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    git(&root, &["branch", "agent/open"]);
    git(&root, &["branch", "agent/checked-out"]);
    git(&root, &["branch", "agent/archived"]);
    git(&root, &["branch", "unrelated"]);
    git(&root, &["switch", "-q", "-c", "agent/unmerged"]);
    std::fs::write(root.join("work.txt"), "unmerged work\n").unwrap();
    git(&root, &["add", "work.txt"]);
    git(&root, &["commit", "-qm", "unmerged work"]);
    git(&root, &["switch", "-q", "main"]);
    let checkout = root.with_file_name("checked-out");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            checkout.to_str().unwrap(),
            "agent/checked-out",
        ],
    );

    fixture.session("agent/merged", false);
    fixture.session("agent/unmerged", false);
    fixture.session("agent/open", true);
    fixture.session("agent/checked-out", false);
    let archived = fixture.session("agent/archived", false);
    let archive_root = fixture.broker.recovery_archive_root().unwrap();
    std::fs::create_dir_all(archive_root.join(format!("{archived}-archive"))).unwrap();

    let summary = fixture.broker.run_merged_branch_lane();

    assert_eq!(summary.skipped, None, "{summary:?}");
    assert_eq!(summary.deleted, 1, "{summary:?}");
    assert_eq!(summary.deleted_total, 1);
    assert_eq!(summary.cleaned_session_branches, 4, "{summary:?}");
    let kept = |reason: &str| summary.kept_by_reason.get(reason).copied();
    assert_eq!(kept("unmerged"), Some(1), "{summary:?}");
    assert_eq!(kept("session_not_cleaned"), Some(1), "{summary:?}");
    assert_eq!(kept("checked_out"), Some(1), "{summary:?}");
    assert_eq!(kept("recovery_archive"), Some(1), "{summary:?}");
    assert!(!branch_exists(&root, "agent/merged"));
    for kept in [
        "agent/unmerged",
        "agent/open",
        "agent/checked-out",
        "agent/archived",
        "unrelated",
        "main",
    ] {
        assert!(branch_exists(&root, kept), "{kept} must stay");
    }
    let advice = status_advice(fixture.broker.store_ref()).unwrap();
    assert_eq!(advice.id, ADVICE_ID);
    assert!(
        advice.summary.contains("4 local branch(es)"),
        "{}",
        advice.summary
    );
}

/// The slice reads every branch's sessions at once: the answer is the
/// per-branch lookup's, including several sessions on one branch, newest
/// first, and no entry for a branch without sessions.
#[test]
fn the_one_scan_session_lookup_answers_as_the_per_branch_lookup() {
    let mut fixture = fixture();
    for (branch, open) in [
        ("agent/two", false),
        ("agent/other", false),
        ("agent/two", true),
        ("agent/not-asked", true),
    ] {
        fixture.session(branch, open);
    }
    let names = ["agent/two", "agent/other", "agent/none"]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let store = fixture.broker.store_ref();
    let found = store.sessions_on_branches(&names).unwrap();
    assert_eq!(
        found.keys().map(String::as_str).collect::<Vec<_>>(),
        ["agent/other", "agent/two"]
    );
    for (branch, sessions) in &found {
        let ids = |sessions: &[crate::Session]| {
            sessions
                .iter()
                .map(|session| session.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(sessions),
            ids(&store.sessions_on_branch(branch).unwrap())
        );
    }
    assert_eq!(found["agent/two"].len(), 2);
}

/// Branches with a session not cleaned take no place in the window: more of
/// them than one window holds, all sorting first, do not delay the merged
/// branch behind them.
#[test]
fn branches_of_uncleaned_sessions_take_no_place_in_the_window() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    let tip = git(&root, &["rev-parse", "HEAD"]);
    for index in 0..=CHECKS_PER_RUN {
        let name = format!("agent/a-{index:04}");
        let path = root.join(".git/refs/heads").join(&name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("{tip}\n")).unwrap();
        fixture.session(&name, true);
    }
    git(&root, &["branch", "agent/z-merged"]);
    fixture.session("agent/z-merged", false);

    let summary = fixture.broker.run_merged_branch_lane();

    assert_eq!(summary.deleted, 1, "{summary:?}");
    assert_eq!(summary.cleaned_session_branches, 1, "{summary:?}");
    assert_eq!(
        summary.kept_by_reason.get("session_not_cleaned").copied(),
        Some(CHECKS_PER_RUN as u64 + 1),
        "{summary:?}"
    );
    assert!(!branch_exists(&root, "agent/z-merged"));
}

/// The slice budget counts from the judging: a listing that outlasts it
/// still leaves the slice its first branch to judge.
#[test]
fn a_listing_longer_than_the_slice_budget_does_not_stop_the_judging() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    fixture.session("agent/merged", false);

    // Long enough that the judging's first check cannot reach it.
    let budget = Duration::from_secs(1);
    SLICE_BUDGET_FOR_TEST.with(|cell| cell.set(Some(budget)));
    let summary = fixture.broker.run_merged_branch_lane_with(&mut || {
        std::thread::sleep(budget + Duration::from_millis(200));
    });
    SLICE_BUDGET_FOR_TEST.with(|cell| cell.set(None));

    assert_eq!(summary.deleted, 1, "{summary:?}");
    assert!(!branch_exists(&root, "agent/merged"));
}

#[test]
fn a_branch_that_moved_after_it_was_judged_is_kept() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/moving"]);
    fixture.session("agent/moving", false);
    let repo = crate::GitRepo::discover(&root).unwrap();
    let judged = git(&root, &["rev-parse", "agent/moving"]);
    // Somebody commits on the branch between the verdict and the delete.
    let tree = git(&root, &["rev-parse", "HEAD^{tree}"]);
    let moved = git(&root, &["commit-tree", &tree, "-p", &judged, "-m", "late"]);
    git(&root, &["update-ref", "refs/heads/agent/moving", &moved]);
    assert!(
        repo.delete_branch_ref_checked("agent/moving", &judged)
            .is_err()
    );
    assert!(branch_exists(&root, "agent/moving"));
}

#[test]
fn the_lane_can_be_switched_off() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    fixture.session("agent/merged", false);
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/config.toml"),
        "[cleanup]\ndelete_merged_session_branches = false\n",
    )
    .unwrap();

    let summary = fixture.broker.run_merged_branch_lane();
    assert_eq!(summary.skipped.as_deref(), Some("disabled"));
    assert_eq!(summary.deleted, 0);
    assert!(branch_exists(&root, "agent/merged"));
}

fn open_session_on(db: &Path, branch: &str, worktree: &Path) {
    let mut store = crate::BrokerStore::open(db).unwrap();
    store
        .register_session(&crate::NewSession {
            worktree_path: worktree.to_string_lossy().into_owned(),
            branch: branch.into(),
            origin: crate::SessionOrigin::Adopted,
            task: Some("reopened".into()),
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        })
        .unwrap();
}

#[test]
fn a_branch_checked_out_while_the_slice_was_listing_is_kept() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    fixture.session("agent/merged", false);
    let checkout = root.with_file_name("reopened");

    // A reuse recreates the checkout on the branch after the slice listed
    // worktrees but before it took the lifecycle claim.
    let summary = fixture.broker.run_merged_branch_lane_with(&mut || {
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                checkout.to_str().unwrap(),
                "agent/merged",
            ],
        );
    });

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert_eq!(
        summary.kept_by_reason.get("checked_out").copied(),
        Some(1),
        "{summary:?}"
    );
    assert!(branch_exists(&root, "agent/merged"));
    assert_eq!(
        git(&checkout, &["symbolic-ref", "HEAD"]),
        "refs/heads/agent/merged"
    );
}

#[test]
fn a_branch_reopened_by_a_session_while_the_slice_was_listing_is_kept() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    fixture.session("agent/merged", false);
    let db = fixture.broker.store_ref().db_path().to_path_buf();
    let reopened = root.with_file_name("reopened-session");

    let summary = fixture.broker.run_merged_branch_lane_with(&mut || {
        open_session_on(&db, "agent/merged", &reopened);
    });

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert_eq!(
        summary.kept_by_reason.get("session_not_cleaned").copied(),
        Some(1),
        "{summary:?}"
    );
    assert!(branch_exists(&root, "agent/merged"));
}

#[test]
fn a_branch_a_bisect_will_return_to_is_kept() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    fixture.session("agent/merged", false);
    // A bisect started from the branch records it to return there.
    std::fs::write(root.join(".git/BISECT_START"), "agent/merged\n").unwrap();

    let summary = fixture.broker.run_merged_branch_lane();

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert_eq!(
        summary.kept_by_reason.get("checked_out").copied(),
        Some(1),
        "{summary:?}"
    );
    assert!(branch_exists(&root, "agent/merged"));
}

#[test]
fn a_branch_whose_session_path_has_a_start_reservation_is_kept() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    fixture.session("agent/merged", false);
    // `Fixture::session` records this path for the session.
    let path = root.with_file_name("agent-merged");
    let base = git(&root, &["rev-parse", "HEAD"]);
    let held = crate::start_reservation::hold_foreign_reservation(
        &root,
        "agent-merged",
        &path,
        "agent/merged",
        &base,
    );

    let summary = fixture.broker.run_merged_branch_lane();
    drop(held);

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert_eq!(
        summary.kept_by_reason.get("start_reserved").copied(),
        Some(1),
        "{summary:?}"
    );
    assert!(branch_exists(&root, "agent/merged"));

    // Once the reservation is gone the merged branch goes.
    let summary = fixture.broker.run_merged_branch_lane();
    assert_eq!(summary.deleted, 1, "{summary:?}");
    assert!(!branch_exists(&root, "agent/merged"));
}

/// Judging needs no lifecycle claim: a slice that finds nothing to delete
/// records its verdicts while a foreground command holds the claim, and
/// one that finds a merged branch yields only its delete.
#[test]
fn the_slice_judges_without_the_lifecycle_claim_and_takes_it_only_to_delete() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    let tree = git(&root, &["rev-parse", "HEAD^{tree}"]);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let ahead = git(
        &root,
        &["commit-tree", &tree, "-p", &head, "-m", "unmerged"],
    );
    git(&root, &["branch", "agent/unmerged", &ahead]);
    fixture.session("agent/unmerged", false);
    let main_root = fixture.broker.main_root().to_path_buf();

    let claim = crate::file_lock::acquire_repository_lifecycle_lock(&main_root).unwrap();
    let summary = fixture.broker.run_merged_branch_lane();
    assert_eq!(summary.skipped, None, "{summary:?}");
    assert_eq!(
        summary.kept_by_reason.get("unmerged").copied(),
        Some(1),
        "{summary:?}"
    );

    git(&root, &["branch", "agent/merged"]);
    fixture.session("agent/merged", false);
    let summary = fixture.broker.run_merged_branch_lane();
    assert_eq!(
        summary.skipped.as_deref(),
        Some("yielded_to_foreground"),
        "{summary:?}"
    );
    assert!(branch_exists(&root, "agent/merged"));
    drop(claim);

    let summary = fixture.broker.run_merged_branch_lane();
    assert_eq!(summary.deleted, 1, "{summary:?}");
    assert!(!branch_exists(&root, "agent/merged"));
}

/// Runs the slice with `between` executed after the unclaimed judging and
/// before the claimed delete.
fn run_with_change_after_judging(
    fixture: &Fixture,
    between: impl FnMut() + 'static,
) -> MergedBranchSummary {
    AFTER_JUDGING.with(|hook| *hook.borrow_mut() = Some(Box::new(between)));
    let summary = fixture.broker.run_merged_branch_lane();
    AFTER_JUDGING.with(|hook| *hook.borrow_mut() = None);
    summary
}

#[test]
fn a_commit_added_after_judging_keeps_the_branch() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    fixture.session("agent/merged", false);
    let judged = git(&root, &["rev-parse", "agent/merged"]);

    let repository = root.clone();
    let summary = run_with_change_after_judging(&fixture, move || {
        let tree = git(&repository, &["rev-parse", "HEAD^{tree}"]);
        let tip = git(&repository, &["rev-parse", "agent/merged"]);
        let late = git(
            &repository,
            &["commit-tree", &tree, "-p", &tip, "-m", "late"],
        );
        git(
            &repository,
            &["update-ref", "refs/heads/agent/merged", &late],
        );
    });

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert_eq!(
        summary.kept_by_reason.get("delete_failed").copied(),
        Some(1),
        "{summary:?}"
    );
    assert!(branch_exists(&root, "agent/merged"));
    assert_ne!(git(&root, &["rev-parse", "agent/merged"]), judged);
}

#[test]
fn a_session_registered_on_the_branch_after_judging_keeps_it() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    fixture.session("agent/merged", false);
    let db = fixture.broker.store_ref().db_path().to_path_buf();
    let reopened = root.with_file_name("reopened-after-judging");

    let summary = run_with_change_after_judging(&fixture, move || {
        open_session_on(&db, "agent/merged", &reopened);
    });

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert_eq!(
        summary.kept_by_reason.get("session_not_cleaned").copied(),
        Some(1),
        "{summary:?}"
    );
    assert!(branch_exists(&root, "agent/merged"));
}

#[test]
fn a_recovery_archive_written_after_judging_keeps_the_branch() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    let session = fixture.session("agent/merged", false);
    let archive_root = fixture.broker.recovery_archive_root().unwrap();

    let summary = run_with_change_after_judging(&fixture, move || {
        std::fs::create_dir_all(archive_root.join(format!("{session}-archive"))).unwrap();
    });

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert_eq!(
        summary.kept_by_reason.get("recovery_archive").copied(),
        Some(1),
        "{summary:?}"
    );
    assert!(branch_exists(&root, "agent/merged"));
}
