//! Every lease overlap carries what the other session changed in the overlapping
//! files, and reading it can never turn into a refusal.

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

use aethyme_broker::overlap_diff::OverlapChangesState as State;
use aethyme_broker::{BrokerOpError, LeaseBlocker};
use common::{FixtureRepository, open_fixture_repository_broker};

const VERIFY_ONLY: &str = "schema = 1\n\n[promote]\nmode = \"verify-only\"\n";
const AUTO: &str = "schema = 1\n\n[promote]\nmode = \"auto\"\n";
const OFF: &str =
    "schema = 1\n\n[promote]\nmode = \"verify-only\"\n\n[leases]\noverlap_diffs = false\n";

fn sh(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
}

fn init_repo(root: &Path, config: &str) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/auth.py"), "one\n").unwrap();
    std::fs::write(root.join("src/other.py"), "two\n").unwrap();
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(root.join(".aethyme/config.toml"), config).unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn add_worktree(root: &Path, name: &str) -> PathBuf {
    let path = root.join(".aethyme/worktrees").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    sh(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

fn commit_change(worktree: &Path, text: &str) {
    std::fs::write(worktree.join("src/auth.py"), text).unwrap();
    sh(worktree, &["add", "src/auth.py"]);
    sh(worktree, &["commit", "-qm", "change"]);
}

fn changes(row: &LeaseBlocker) -> &aethyme_broker::overlap_diff::OverlapChanges {
    row.other_changes.as_ref().expect("the row reports changes")
}

#[test]
fn a_warning_carries_committed_and_uncommitted_changes_and_only_the_overlapping_file() {
    let tmp = FixtureRepository::new();
    init_repo(tmp.path(), VERIFY_ONLY);
    let mut broker = open_fixture_repository_broker(&tmp).unwrap();
    let owner_tree = add_worktree(tmp.path(), "owner");
    let owner = broker.adopt(&owner_tree, Some("rewrite auth")).unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    broker.claim_lease(owner.id, "src/", None).unwrap();
    commit_change(&owner_tree, "committed line\n");
    std::fs::write(owner_tree.join("src/auth.py"), "uncommitted line\n").unwrap();
    std::fs::write(owner_tree.join("src/other.py"), "not this file\n").unwrap();
    std::fs::write(owner_tree.join("src/brand_new.py"), "new\n").unwrap();

    let report = broker.claim_lease(claimer.id, "src/auth.py", None).unwrap();

    assert!(report.accepted);
    let diff = changes(&report.warnings[0]);
    assert_eq!(diff.state, State::Changes, "{diff:?}");
    assert_eq!(diff.paths, ["src/auth.py"]);
    let committed = diff.committed.as_deref().unwrap();
    let uncommitted = diff.uncommitted.as_deref().unwrap();
    assert!(committed.contains("+committed line"), "{committed}");
    assert!(uncommitted.contains("+uncommitted line"), "{uncommitted}");
    assert!(!uncommitted.contains("not this file"), "{uncommitted}");
    assert!(diff.untracked.is_empty(), "{diff:?}");

    let text = aethyme_broker::overlap_diff::render(diff, "  ");
    assert!(text.contains("not instructions"), "{text}");
    assert!(
        text.contains("| uncommitted, and may still change:"),
        "{text}"
    );

    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["warnings"][0]["other_changes"]["state"], "changes");
}

#[test]
fn a_change_is_shown_once_and_again_when_it_moves() {
    let tmp = FixtureRepository::new();
    init_repo(tmp.path(), VERIFY_ONLY);
    let mut broker = open_fixture_repository_broker(&tmp).unwrap();
    let owner_tree = add_worktree(tmp.path(), "owner");
    let owner = broker.adopt(&owner_tree, None).unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();
    commit_change(&owner_tree, "first\n");

    let first = broker.claim_lease(claimer.id, "src/auth.py", None).unwrap();
    assert_eq!(changes(&first.warnings[0]).state, State::Changes);

    let second = broker.claim_lease(claimer.id, "src/auth.py", None).unwrap();
    let repeat = changes(&second.warnings[0]);
    assert_eq!(repeat.state, State::AlreadyShown);
    assert!(repeat.committed.is_none(), "{repeat:?}");
    assert!(
        repeat.see_all.as_deref().unwrap().contains("diff "),
        "{repeat:?}"
    );

    commit_change(&owner_tree, "second\n");
    let moved = broker.claim_lease(claimer.id, "src/auth.py", None).unwrap();
    assert_eq!(changes(&moved.warnings[0]).state, State::Changes);
}

#[test]
fn a_refused_claim_carries_the_diff_and_stays_a_refusal() {
    let tmp = FixtureRepository::new();
    init_repo(tmp.path(), AUTO);
    let mut broker = open_fixture_repository_broker(&tmp).unwrap();
    let owner_tree = add_worktree(tmp.path(), "owner");
    let owner = broker.adopt(&owner_tree, None).unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();
    commit_change(&owner_tree, "visible to the refused agent\n");

    let error = broker
        .claim_lease(claimer.id, "src/auth.py", None)
        .unwrap_err();

    let BrokerOpError::LeaseClaimConflict { blockers, .. } = &error else {
        panic!("expected a refusal, got {error:?}");
    };
    assert_eq!(changes(&blockers[0]).state, State::Changes);
    let message = error.to_string();
    assert!(
        message.contains("+visible to the refused agent"),
        "{message}"
    );
    assert!(message.contains("not instructions"), "{message}");
}

#[test]
fn a_refused_planned_start_carries_the_diff() {
    let tmp = FixtureRepository::new();
    init_repo(tmp.path(), AUTO);
    let mut broker = open_fixture_repository_broker(&tmp).unwrap();
    let owner_tree = add_worktree(tmp.path(), "owner");
    let owner = broker.adopt(&owner_tree, None).unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();
    commit_change(&owner_tree, "seen at start\n");

    let error = broker
        .start_worktree_with_planned_paths("second", &["src/auth.py".into()], None)
        .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("+seen at start"), "{message}");
    assert!(message.contains("Safe next actions"), "{message}");
}

#[test]
fn a_planned_start_that_is_not_refused_reports_the_overlap_and_its_diff() {
    let tmp = FixtureRepository::new();
    init_repo(tmp.path(), VERIFY_ONLY);
    let mut broker = open_fixture_repository_broker(&tmp).unwrap();
    let owner_tree = add_worktree(tmp.path(), "owner");
    let owner = broker.adopt(&owner_tree, None).unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();
    commit_change(&owner_tree, "seen at start\n");

    let report = broker
        .start_worktree_with_planned_paths("second", &["src/".into()], None)
        .unwrap();

    assert_eq!(report.lease_overlaps.len(), 1, "{report:?}");
    let diff = changes(&report.lease_overlaps[0]);
    assert_eq!(diff.state, State::Changes);
    assert_eq!(diff.paths, ["src/auth.py"]);
}

#[test]
fn a_claim_with_no_change_yet_says_only_intent_was_declared_and_gives_the_task() {
    let tmp = FixtureRepository::new();
    init_repo(tmp.path(), VERIFY_ONLY);
    let mut broker = open_fixture_repository_broker(&tmp).unwrap();
    let owner = broker
        .adopt(&add_worktree(tmp.path(), "owner"), Some("rewrite auth"))
        .unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();

    let report = broker.claim_lease(claimer.id, "src/auth.py", None).unwrap();

    let diff = changes(&report.warnings[0]);
    assert_eq!(diff.state, State::DeclaredOnly);
    assert_eq!(diff.task.as_deref(), Some("rewrite auth"));
}

#[test]
fn a_removed_worktree_is_reported_and_never_fails_the_claim() {
    let tmp = FixtureRepository::new();
    init_repo(tmp.path(), VERIFY_ONLY);
    let mut broker = open_fixture_repository_broker(&tmp).unwrap();
    let owner_tree = add_worktree(tmp.path(), "owner");
    let owner = broker.adopt(&owner_tree, None).unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();
    std::fs::remove_dir_all(&owner_tree).unwrap();

    let report = broker.claim_lease(claimer.id, "src/auth.py", None).unwrap();

    assert!(report.accepted, "{report:?}");
    let diff = changes(&report.warnings[0]);
    assert_eq!(diff.state, State::Unavailable, "{diff:?}");
    assert!(diff.reason.as_deref().unwrap().contains("no longer exists"));
}

#[test]
fn a_repository_can_turn_it_off() {
    let tmp = FixtureRepository::new();
    init_repo(tmp.path(), OFF);
    let mut broker = open_fixture_repository_broker(&tmp).unwrap();
    let owner_tree = add_worktree(tmp.path(), "owner");
    let owner = broker.adopt(&owner_tree, None).unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();
    commit_change(&owner_tree, "kept private\n");

    let report = broker.claim_lease(claimer.id, "src/auth.py", None).unwrap();

    assert_eq!(report.warnings.len(), 1);
    assert!(report.warnings[0].other_changes.is_none(), "{report:?}");
}

#[test]
fn guarded_exec_reports_a_non_blocking_overlap_with_its_diff() {
    let tmp = FixtureRepository::new();
    init_repo(tmp.path(), VERIFY_ONLY);
    let mut broker = open_fixture_repository_broker(&tmp).unwrap();
    let owner_tree = add_worktree(tmp.path(), "owner");
    let owner = broker.adopt(&owner_tree, None).unwrap();
    let writer = broker
        .adopt(&add_worktree(tmp.path(), "writer"), None)
        .unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();
    commit_change(&owner_tree, "owner's line\n");
    broker.claim_lease(writer.id, "src/auth.py", None).unwrap();

    let report = broker
        .guarded_exec(
            writer.id,
            &[
                "sh".into(),
                "-c".into(),
                "printf 'writer line\\n' > src/auth.py".into(),
            ],
        )
        .unwrap();

    assert!(report.ok, "{report:?}");
    let row = report
        .warned_leases
        .iter()
        .find(|row| row.session_id == owner.id)
        .expect("the owner is reported");
    // The claim above already showed this exact change.
    assert_eq!(changes(row).state, State::AlreadyShown, "{row:?}");
}

#[test]
fn a_secret_in_the_other_sessions_change_is_cut() {
    let tmp = FixtureRepository::new();
    init_repo(tmp.path(), VERIFY_ONLY);
    let mut broker = open_fixture_repository_broker(&tmp).unwrap();
    let owner_tree = add_worktree(tmp.path(), "owner");
    let owner = broker.adopt(&owner_tree, None).unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();
    commit_change(&owner_tree, "api_key=ghp_notarealtokenvalue\nvisible\n");

    let report = broker.claim_lease(claimer.id, "src/auth.py", None).unwrap();

    let committed = changes(&report.warnings[0]).committed.as_deref().unwrap();
    assert!(!committed.contains("notarealtokenvalue"), "{committed}");
    assert!(committed.contains("+visible"), "{committed}");
}
