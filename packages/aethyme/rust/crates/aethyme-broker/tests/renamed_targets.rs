//! Issue #145: a session whose target paths were renamed by work that landed
//! after it started must be told before its replay fails as a modify/delete
//! conflict. Work lands by merged pull request; a promoting repository keeps
//! reporting renames promotion made.

use std::path::Path;
use std::process::Command;

mod common;

use aethyme_broker::AdoptMode;
use common::{FixtureRepository, open_fixture_repository_broker};

fn sh(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// How the moving session's work lands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Landing {
    /// Promoted to integration on submit.
    Promotion,
    /// A squash-merged pull request into the fetched default branch.
    PullRequest,
}

/// A repo where one session's landed work has already moved
/// `plugins/old/tool.py` to `plugins/new/tool.py`, and a second session still
/// targets the old path.
fn fixture(rename: bool, landing: Landing) -> (FixtureRepository, std::path::PathBuf, i64) {
    let fixture = FixtureRepository::new();
    let repo = fixture.path().to_path_buf();
    std::fs::create_dir_all(repo.join("plugins/old")).unwrap();
    sh(&repo, &["init", "-q", "-b", "main"]);
    if landing == Landing::Promotion {
        aethyme_testkit::promote_on_submit(&repo);
    }
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(
        repo.join("plugins/old/tool.py"),
        "def run():\n    return 1\n",
    )
    .unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "init"]);
    if landing == Landing::PullRequest {
        aethyme_testkit::ensure_origin(&repo);
    }

    let mut broker = open_fixture_repository_broker(&fixture).unwrap();

    // The session that still targets the old path, branched before the move.
    let stale = broker.start_worktree("edit the old path", None).unwrap();
    let stale_worktree = std::path::PathBuf::from(&stale.worktree_path);
    std::fs::write(
        stale_worktree.join("plugins/old/tool.py"),
        "def run():\n    return 2\n",
    )
    .unwrap();
    sh(&stale_worktree, &["add", "-A"]);
    sh(&stale_worktree, &["commit", "-qm", "tweak the tool"]);

    // A different session generalizes the plugin, and lands first.
    let mover = broker
        .start_worktree("generalize the plugin", None)
        .unwrap();
    let mover_worktree = std::path::PathBuf::from(&mover.worktree_path);
    if rename {
        std::fs::create_dir_all(mover_worktree.join("plugins/new")).unwrap();
        sh(
            &mover_worktree,
            &["mv", "plugins/old/tool.py", "plugins/new/tool.py"],
        );
    } else {
        sh(&mover_worktree, &["rm", "-q", "plugins/old/tool.py"]);
    }
    sh(&mover_worktree, &["add", "-A"]);
    sh(&mover_worktree, &["commit", "-qm", "generalize the plugin"]);
    match landing {
        Landing::Promotion => assert!(broker.submit(mover.id).unwrap().promoted),
        Landing::PullRequest => {
            aethyme_testkit::land_as_pull_request(
                &repo,
                &mover.branch,
                aethyme_testkit::MergeStyle::Squash,
            );
        }
    }

    (fixture, repo, stale.id)
}

#[test]
fn a_rename_landed_by_a_pull_request_is_named_with_its_new_path() {
    let (fixture, _repo, stale_id) = fixture(true, Landing::PullRequest);
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();

    let renamed = broker.session_renamed_targets(stale_id).unwrap();
    assert_eq!(renamed.len(), 1, "expected one renamed target: {renamed:?}");
    assert_eq!(renamed[0].from, "plugins/old/tool.py");
    assert_eq!(renamed[0].to, "plugins/new/tool.py");
    assert_eq!(
        renamed[0].promoted_entry_id, None,
        "no promotion moved it: {renamed:?}"
    );
}

/// A promoting repository still judges renames against integration.
#[test]
fn a_promoted_rename_is_named_with_its_new_path_and_owning_entry() {
    let (fixture, _repo, stale_id) = fixture(true, Landing::Promotion);
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();

    let renamed = broker.session_renamed_targets(stale_id).unwrap();
    assert_eq!(renamed.len(), 1, "expected one renamed target: {renamed:?}");
    assert_eq!(renamed[0].from, "plugins/old/tool.py");
    assert_eq!(renamed[0].to, "plugins/new/tool.py");
    assert!(
        renamed[0].promoted_entry_id.is_some(),
        "the promotion that moved it must be named: {renamed:?}"
    );
}

/// A genuine deletion must stay a deletion. Reporting it as a rename would send
/// the operator to a path that does not exist.
#[test]
fn a_genuine_deletion_is_not_reported_as_a_rename() {
    let (fixture, _repo, stale_id) = fixture(false, Landing::PullRequest);
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    assert!(
        broker.session_renamed_targets(stale_id).unwrap().is_empty(),
        "a deleted path must not be reported as renamed"
    );
}

/// Nothing is reported when the session's paths still exist.
#[test]
fn an_unaffected_session_reports_nothing() {
    let fixture = FixtureRepository::new();
    let repo = fixture.path();
    sh(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.join("keep.txt"), "stable\n").unwrap();
    sh(repo, &["add", "-A"]);
    sh(repo, &["commit", "-qm", "init"]);
    aethyme_testkit::ensure_origin(repo);

    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    let session = broker.start_worktree("ordinary work", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("keep.txt"), "edited\n").unwrap();
    sh(&worktree, &["add", "-A"]);
    sh(&worktree, &["commit", "-qm", "edit"]);

    assert!(
        broker
            .session_renamed_targets(session.id)
            .unwrap()
            .is_empty()
    );
}

/// The advisory reaches the operator at adopt, before a replay is attempted.
#[test]
fn adopt_surfaces_the_rename_before_a_replay_is_attempted() {
    let (fixture, repo, _) = fixture(true, Landing::PullRequest);
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();

    // A hand-made worktree that still targets the old path.
    let manual = repo.parent().unwrap().join("manual");
    sh(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "manual",
            manual.to_str().unwrap(),
        ],
    );
    let report = broker
        .adopt_with(&manual, Some("port the tool"), AdoptMode::New, None)
        .unwrap();
    // Nothing touched yet, so nothing to report.
    assert!(report.renamed_targets.is_empty());
}
