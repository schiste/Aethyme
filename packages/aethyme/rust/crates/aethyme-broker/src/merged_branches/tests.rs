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

/// Runs one slice and returns its summary and its telemetry counts.
fn run_counted(fixture: &Fixture) -> (MergedBranchSummary, BTreeMap<String, u64>) {
    let recorder = crate::phase_telemetry::Recorder::default();
    let summary = {
        let _installed = crate::phase_telemetry::install(recorder.clone());
        fixture.broker.run_merged_branch_lane()
    };
    (summary, recorder.totals().1)
}

/// The judging reads the archive directories once for the slice; each branch
/// about to be deleted still gets its own fresh reading under the claim.
#[test]
fn a_slice_lists_the_recovery_archives_once_and_each_delete_reads_them_afresh() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    for branch in ["agent/a", "agent/b", "agent/c"] {
        git(&root, &["branch", branch]);
        fixture.session(branch, false);
    }
    let archive_root = fixture.broker.recovery_archive_root().unwrap();
    std::fs::create_dir_all(archive_root.join("999999-other")).unwrap();

    let (summary, counts) = run_counted(&fixture);

    assert_eq!(summary.deleted, 3, "{summary:?}");
    let count = |name: &str| counts.get(name).copied().unwrap_or(0);
    assert_eq!(count("merged_branches_archive_listing"), 1, "{counts:?}");
    assert_eq!(
        count("merged_branches_archive_listing_fresh"),
        3,
        "{counts:?}"
    );
    // One entry, read by the slice's listing and by each fresh reading.
    assert_eq!(count("merged_branches_archive_entries"), 4, "{counts:?}");
}

/// Only a finished archive whose name starts with exactly the session's id
/// counts, whichever way the names are read.
#[test]
fn the_slice_listing_answers_as_the_fresh_reading() {
    let mut fixture = fixture();
    let ids = ["agent/a", "agent/b", "agent/c"].map(|branch| fixture.session(branch, false));
    let archive_root = fixture.broker.recovery_archive_root().unwrap();
    for name in [
        format!("{}-archive", ids[0]),
        format!("{}-archive.partial", ids[1]),
        format!("{}0-archive", ids[2]),
        format!("{}.retirement-v6", ids[2]),
        format!(".partial-operation-{}-x", ids[2]),
    ] {
        std::fs::create_dir_all(archive_root.join(name)).unwrap();
    }
    let archives = SliceArchives::default();
    for (id, expected) in ids.into_iter().zip([true, false, false]) {
        let session = fixture.broker.store_ref().session(id).unwrap();
        assert_eq!(
            fixture.broker.has_restorable_state(&session),
            Some(expected)
        );
        assert_eq!(
            fixture
                .broker
                .has_restorable_state_with(&session, |owner| archives
                    .holds(&fixture.broker, owner)),
            Some(expected)
        );
    }
}

/// A retired spawned session keeps its branch for `recovery_archive_days`
/// after cleanup even when its tombstone is gone.
#[test]
fn a_cleaned_spawned_session_keeps_its_branch_until_the_retention_age() {
    let mut fixture = fixture();
    let id = fixture.session("agent/retained", false);
    let set = |completed: i64| {
        fixture
            .broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET origin = 'spawned', cleanup_completed_at = ?1 WHERE id = ?2",
                rusqlite::params![completed, id],
            )
            .unwrap();
    };
    let judge = || {
        let session = fixture.broker.store_ref().session(id).unwrap();
        fixture
            .broker
            .has_restorable_state_with(&session, |_| Some(false))
    };
    set(crate::clock::epoch_ms() - 3_600_000);
    assert_eq!(judge(), Some(true), "no tombstone, cleaned an hour ago");
    set(crate::clock::epoch_ms() - 100 * 86_400_000);
    assert_eq!(judge(), Some(false), "past the retention age");
}

/// An archive directory that cannot be listed is unknown, not empty: every
/// branch that needs it is kept, and none is taken to the claimed re-judge.
#[test]
fn an_unreadable_archive_directory_keeps_every_branch_of_the_slice() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    for branch in ["agent/a", "agent/b"] {
        git(&root, &["branch", branch]);
        fixture.session(branch, false);
    }
    let archive_root = fixture.broker.recovery_archive_root().unwrap();
    std::fs::create_dir_all(archive_root.parent().unwrap()).unwrap();
    std::fs::write(&archive_root, "not a directory\n").unwrap();

    let (summary, counts) = run_counted(&fixture);

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert_eq!(
        summary.kept_by_reason.get("recovery_archive").copied(),
        Some(2),
        "{summary:?}"
    );
    assert!(
        !counts.contains_key("merged_branches_archive_listing_fresh"),
        "{counts:?}"
    );
    assert!(branch_exists(&root, "agent/a"));
    assert!(branch_exists(&root, "agent/b"));
}

/// The listing is read only for a session nothing cheaper keeps, and the
/// tombstone check stays per session.
#[test]
fn a_slice_whose_sessions_all_left_tombstones_lists_no_archives() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/retired"]);
    let id = fixture.session("agent/retired", false);
    let session = fixture.broker.store_ref().session(id).unwrap();
    std::fs::create_dir_all(&session.worktree_path).unwrap();
    std::fs::write(
        Path::new(&session.worktree_path).join("ARCHIVED-SESSION.txt"),
        "aethyme: retired session checkout\n",
    )
    .unwrap();

    let (summary, counts) = run_counted(&fixture);

    assert_eq!(
        summary.kept_by_reason.get("recovery_archive").copied(),
        Some(1),
        "{summary:?}"
    );
    assert!(
        !counts.contains_key("merged_branches_archive_listing"),
        "{counts:?}"
    );
    assert!(branch_exists(&root, "agent/retired"));
}

/// Binds the repository's pool (by starting a session there) and returns it.
fn bind_pool(fixture: &mut Fixture) -> std::path::PathBuf {
    git(&fixture.root, &["config", "user.name", "t"]);
    git(
        &fixture.root,
        &["config", "user.email", "t@example.invalid"],
    );
    fixture
        .broker
        .start_worktree("bind the pool", None)
        .unwrap();
    fixture
        .broker
        .validate_bound_repository_worktree_root()
        .unwrap()
        .expect("the pool is bound")
}

/// A merged branch of a cleaned session; the session's id.
fn merged_branch(fixture: &mut Fixture) -> i64 {
    git(&fixture.root, &["branch", "agent/merged"]);
    fixture.session("agent/merged", false)
}

fn kept(summary: &MergedBranchSummary, reason: &str) -> Option<u64> {
    summary.kept_by_reason.get(reason).copied()
}

/// With no pool bound the host root is the only one, and nothing is unknown.
#[test]
fn a_repository_with_no_pool_bound_still_deletes() {
    let mut fixture = fixture();
    merged_branch(&mut fixture);
    let roots = fixture.broker.checked_recovery_archive_roots().unwrap();
    assert_eq!(
        roots.into_iter().map(|root| root.path).collect::<Vec<_>>(),
        [fixture.broker.recovery_archive_root().unwrap()]
    );

    let (summary, _) = run_counted(&fixture);

    assert_eq!(summary.deleted, 1, "{summary:?}");
}

/// A bound pool that never held an archive adds no root and nothing unknown.
#[test]
fn a_bound_pool_without_an_archive_directory_still_deletes() {
    let mut fixture = fixture();
    bind_pool(&mut fixture);
    merged_branch(&mut fixture);

    let (summary, _) = run_counted(&fixture);

    assert_eq!(summary.deleted, 1, "{summary:?}");
}

/// An archive held only by the pool keeps the branch, also when the archive
/// directory's marker names another repository.
#[test]
fn an_archive_only_on_the_pool_keeps_the_branch() {
    let mut fixture = fixture();
    bind_pool(&mut fixture);
    let session = merged_branch(&mut fixture);
    let pool_root = fixture.broker.pool_recovery_archive_root(true).unwrap();
    std::fs::create_dir_all(pool_root.join(format!("{session}-archive"))).unwrap();
    let recovery_marker = pool_root
        .parent()
        .unwrap()
        .join(".aethyme-recovery-archives-root.json");
    for other_owner in [false, true] {
        if other_owner {
            std::fs::write(&recovery_marker, r#"{"repository_key":"other"}"#).unwrap();
        }

        let (summary, counts) = run_counted(&fixture);

        assert_eq!(summary.deleted, 0, "{summary:?}");
        assert_eq!(kept(&summary, "recovery_archive"), Some(1), "{summary:?}");
        assert!(
            !counts.contains_key("merged_branches_archive_roots_unknown"),
            "{counts:?}"
        );
    }
}

/// A bound pool that is not there, or whose archive marker cannot be read, is
/// unknown to the judging, not empty.
#[test]
fn an_unresolved_pool_root_keeps_the_branch_when_judging() {
    let mut fixture = fixture();
    let pool = bind_pool(&mut fixture);
    merged_branch(&mut fixture);
    let pool_root = fixture.broker.pool_recovery_archive_root(true).unwrap();
    let marker = pool_root
        .parent()
        .unwrap()
        .join(".aethyme-recovery-archives-root.json");
    let readable = std::fs::read(&marker).unwrap();
    let away = pool.with_file_name("pool-away");
    for pool_away in [false, true] {
        if pool_away {
            std::fs::write(&marker, &readable).unwrap();
            std::fs::rename(&pool, &away).unwrap();
        } else {
            std::fs::write(&marker, "not json").unwrap();
        }

        let (summary, counts) = run_counted(&fixture);

        assert_eq!(summary.deleted, 0, "{summary:?}");
        assert_eq!(kept(&summary, "recovery_archive"), Some(1), "{summary:?}");
        assert_eq!(
            counts.get("merged_branches_archive_roots_unknown"),
            Some(&1),
            "{counts:?}"
        );
        assert!(
            !counts.contains_key("merged_branches_archive_listing_fresh"),
            "{counts:?}"
        );
    }
    std::fs::rename(&away, &pool).unwrap();
    let (summary, _) = run_counted(&fixture);
    assert_eq!(summary.deleted, 1, "{summary:?}");
}

/// The same under the claim: the pool went away after the judging.
#[test]
fn an_unresolved_pool_root_keeps_the_branch_under_the_claim() {
    let mut fixture = fixture();
    let pool = bind_pool(&mut fixture);
    merged_branch(&mut fixture);
    let away = pool.with_file_name("pool-away");

    let summary = run_with_change_after_judging(&fixture, move || {
        std::fs::rename(&pool, &away).unwrap();
    });

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert_eq!(kept(&summary, "unknown"), Some(1), "{summary:?}");
    assert!(branch_exists(&fixture.root, "agent/merged"));
}

/// The session's archive record: one that cannot be read, or whose archive
/// directory's parent is gone, keeps the branch; an archive known to be gone
/// does not.
#[test]
fn an_archive_record_that_cannot_be_checked_keeps_the_branch() {
    let mut fixture = fixture();
    let session = merged_branch(&mut fixture);
    let parent = fixture.root.with_file_name("old-archives");
    let recorded = serde_json::json!({
        (session.to_string()): parent.join("archive").to_string_lossy()
    })
    .to_string();
    for text in ["not json", recorded.as_str()] {
        fixture
            .broker
            .store_ref()
            .meta_set("checkout_retirement.archived_sessions", text)
            .unwrap();
        let (summary, _) = run_counted(&fixture);
        assert_eq!(summary.deleted, 0, "{summary:?}");
        assert_eq!(kept(&summary, "recovery_archive"), Some(1), "{summary:?}");
    }
    std::fs::create_dir_all(&parent).unwrap();
    let (summary, _) = run_counted(&fixture);
    assert_eq!(summary.deleted, 1, "{summary:?}");
}

/// Host state not created yet is reachable; host state on a volume that is
/// not there is not.
#[test]
fn host_state_is_reachable_when_present_or_only_not_created_yet() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("state");
    let anchor = crate::cleanup_resolve::host_state_anchor;
    assert_eq!(anchor(&base), Some(dir.path()));
    assert_eq!(anchor(&base.join("nested")), None);
    std::fs::write(&base, "a file").unwrap();
    assert_eq!(anchor(&base), None);
    std::fs::remove_file(&base).unwrap();
    std::fs::create_dir(&base).unwrap();
    assert_eq!(anchor(&base), Some(base.as_path()));
}

/// A dangling link at the archive root or at the directory above it is
/// unknown, not empty; with the links gone the branch is deleted.
#[test]
fn a_dangling_link_on_the_way_to_the_archives_keeps_the_branch() {
    let mut fixture = fixture();
    merged_branch(&mut fixture);
    let archive_root = fixture.broker.recovery_archive_root().unwrap();
    let above = archive_root.parent().unwrap().to_path_buf();
    let gone = fixture.root.with_file_name("gone");
    for link in [&above, &archive_root] {
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&gone, link).unwrap();

        let (summary, _) = run_counted(&fixture);

        assert_eq!(summary.deleted, 0, "{summary:?}");
        assert_eq!(kept(&summary, "recovery_archive"), Some(1), "{summary:?}");
        std::fs::remove_file(link).unwrap();
    }
    let (summary, _) = run_counted(&fixture);
    assert_eq!(summary.deleted, 1, "{summary:?}");
}

/// A root reads as empty only while its anchor is still a directory and
/// nothing between the two is in the way.
#[test]
fn a_root_is_plainly_absent_only_below_a_directory_that_is_still_there() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = dir.path().join("state");
    let root = crate::cleanup_resolve::CheckedArchiveRoot {
        path: anchor.join("recovery-archives/key"),
        anchor: anchor.clone(),
    };
    // The anchor went away since the root was resolved.
    assert!(!root.plainly_absent());
    std::os::unix::fs::symlink(dir.path().join("gone"), &anchor).unwrap();
    assert!(!root.plainly_absent());
    std::fs::remove_file(&anchor).unwrap();
    std::fs::create_dir(&anchor).unwrap();
    assert!(root.plainly_absent());
    std::fs::write(anchor.join("recovery-archives"), "a file").unwrap();
    assert!(!root.plainly_absent());
    std::fs::remove_file(anchor.join("recovery-archives")).unwrap();
    std::fs::create_dir(anchor.join("recovery-archives")).unwrap();
    assert!(root.plainly_absent());
    // There after all: the caller read it too early to say.
    std::fs::create_dir(&root.path).unwrap();
    assert!(!root.plainly_absent());
}

/// An adopted storage root beside the fixture's host state, and this
/// repository's archive directory on it (`storage.toml` `[placement]`).
fn adopt_storage_root(fixture: &Fixture) -> (std::path::PathBuf, std::path::PathBuf) {
    let host_state = fixture._storage.host_state_dir();
    std::fs::create_dir_all(&host_state).unwrap();
    let storage = fixture.root.with_file_name("storage-root");
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::write(
        storage.join(crate::storage_ledger::STORAGE_ROOT_MARKER),
        "adopted\n",
    )
    .unwrap();
    crate::storage_ledger::check::register_root(
        &host_state,
        &storage,
        crate::storage_ledger::RootKind::Storage,
        None,
    );
    let host_root = fixture.broker.recovery_archive_root().unwrap();
    let key = host_root.file_name().unwrap();
    let archive_dir = storage.join("recovery-archives").join(key);
    (storage, archive_dir)
}

/// Readers find an archive on an adopted storage root as they do in host
/// state: it is among the roots, and it keeps its session's branch.
#[test]
fn an_archive_only_on_a_storage_root_keeps_the_branch() {
    let mut fixture = fixture();
    let session = merged_branch(&mut fixture);
    let (_, archive_dir) = adopt_storage_root(&fixture);
    std::fs::create_dir_all(archive_dir.join(format!("{session}-archive"))).unwrap();
    assert!(
        fixture
            .broker
            .recovery_archive_roots()
            .contains(&archive_dir)
    );
    let checked = fixture.broker.checked_recovery_archive_roots().unwrap();
    assert!(checked.iter().any(|root| root.path == archive_dir));

    let (summary, counts) = run_counted(&fixture);

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert_eq!(kept(&summary, "recovery_archive"), Some(1), "{summary:?}");
    assert!(
        !counts.contains_key("merged_branches_archive_roots_unknown"),
        "{counts:?}"
    );
}

/// A storage root that received archives and is away (its volume
/// unmounted) may hold some: nothing is judged absent. One that never did
/// does not hold deletions back.
#[test]
fn a_storage_root_that_received_archives_and_is_away_keeps_the_branch() {
    let mut fixture = fixture();
    merged_branch(&mut fixture);
    let (storage, _) = adopt_storage_root(&fixture);
    let away = storage.with_file_name("storage-away");
    std::fs::rename(&storage, &away).unwrap();
    assert!(
        fixture.broker.checked_recovery_archive_roots().is_some(),
        "an adopted root that never received archives is not needed"
    );
    crate::storage_ledger::placement::record_archive_storage_root(
        &fixture._storage.host_state_dir(),
        &storage,
    )
    .unwrap();
    assert!(fixture.broker.checked_recovery_archive_roots().is_none());

    let (summary, counts) = run_counted(&fixture);

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert!(
        counts.contains_key("merged_branches_archive_roots_unknown"),
        "{counts:?}"
    );
    std::fs::rename(&away, &storage).unwrap();
    let (summary, _) = run_counted(&fixture);
    assert_eq!(summary.deleted, 1, "{summary:?}");
}

/// New archives go to the storage root while placement says so and it
/// qualifies; with the root away they go to host state instead.
#[test]
fn new_archives_follow_the_placement_and_fall_back_to_host_state() {
    let fixture = fixture();
    let (storage, archive_dir) = adopt_storage_root(&fixture);
    let host_root = fixture.broker.recovery_archive_root().unwrap();
    assert_eq!(
        fixture.broker.placed_recovery_archive_root().unwrap(),
        host_root,
        "the default is host state"
    );
    std::fs::write(
        fixture._storage.host_state_dir().join("storage.toml"),
        "[placement]\nrecovery_archives = \"storage\"\n",
    )
    .unwrap();
    crate::storage_ledger::placement::set_local_for_test(Some(true));
    assert_eq!(
        fixture.broker.placed_recovery_archive_root().unwrap(),
        archive_dir
    );
    let away = storage.with_file_name("storage-away");
    std::fs::rename(&storage, &away).unwrap();
    assert_eq!(
        fixture.broker.placed_recovery_archive_root().unwrap(),
        host_root
    );
    crate::storage_ledger::placement::set_local_for_test(None);
}

/// The operation archive lane takes its root from `retirement_archive_root`
/// and writes there without the resolve path's recording: resolving a
/// storage root records it, so an archive written there and then made
/// unreachable (an unmounted volume) defers absence and keeps the branch.
#[test]
fn an_operation_archive_on_an_unmounted_storage_root_keeps_the_branch() {
    let mut fixture = fixture();
    let session = merged_branch(&mut fixture);
    let (storage, archive_dir) = adopt_storage_root(&fixture);
    std::fs::write(
        fixture._storage.host_state_dir().join("storage.toml"),
        "[placement]\nrecovery_archives = \"storage\"\n",
    )
    .unwrap();
    crate::storage_ledger::placement::set_local_for_test(Some(true));
    // No pool is bound: the lane's root is the placed one.
    let lane_root = fixture.broker.retirement_archive_root().unwrap();
    assert_eq!(lane_root, archive_dir);
    std::fs::create_dir_all(lane_root.join(format!("{session}-operation-archive"))).unwrap();
    let away = storage.with_file_name("storage-away");
    std::fs::rename(&storage, &away).unwrap();

    assert!(fixture.broker.checked_recovery_archive_roots().is_none());
    let (summary, counts) = run_counted(&fixture);

    assert_eq!(summary.deleted, 0, "{summary:?}");
    assert!(
        counts.contains_key("merged_branches_archive_roots_unknown"),
        "{counts:?}"
    );
    assert!(branch_exists(&fixture.root, "agent/merged"));
    crate::storage_ledger::placement::set_local_for_test(None);
}
