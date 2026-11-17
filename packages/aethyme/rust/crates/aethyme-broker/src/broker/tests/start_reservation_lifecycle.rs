//! `broker start` holds the repository lifecycle lock only while it reserves
//! its checkout; `git worktree add` runs under the per-checkout reservation.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// What the hook saw: lock free, destination reserved, destination, the
/// concurrent start's checkout.
type HookObservation = Rc<RefCell<Option<(bool, bool, PathBuf, PathBuf)>>>;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
        vec!["commit", "--allow-empty", "-qm", "init"],
    ] {
        git(repo.path(), &args);
    }
    repo
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(repo)
        .status()
        .unwrap()
        .success()
}

#[test]
fn a_second_start_proceeds_while_the_first_is_in_worktree_add() {
    let repo = repository();
    let storage = crate::test_storage::TestStorage::new();
    let options = storage.options();
    let mut first = storage.open(repo.path()).unwrap();

    let observed: HookObservation = Rc::default();
    let observed_in_hook = Rc::clone(&observed);
    let repo_path = repo.path().to_path_buf();
    crate::start_reservation::set_before_add_hook(Box::new(
        move |destination: &Path, _branch: &str| {
            // The wide lock is free while this start is about to run `worktree add`.
            let lock_free = {
                let _foreground = crate::file_lock::ForegroundLifecycleTryClaim::enter();
                crate::file_lock::try_acquire_repository_lifecycle_lock(&repo_path, "test")
                    .unwrap()
                    .is_some()
            };
            let reserved = crate::start_reservation::path_is_reserved(&repo_path, destination);
            // Another start with the same task name runs to completion meanwhile
            // and must choose a different checkout.
            let mut second = crate::Broker::open_with_options(&repo_path, options).unwrap();
            let session = second.start_worktree("same task", None).unwrap();
            *observed_in_hook.borrow_mut() = Some((
                lock_free,
                reserved,
                destination.to_path_buf(),
                PathBuf::from(session.worktree_path),
            ));
        },
    ));

    let session = first.start_worktree("same task", None).unwrap();
    let (lock_free, reserved, destination, second_path) =
        observed.borrow_mut().take().expect("the hook ran");
    assert!(
        lock_free,
        "start held the lifecycle lock during worktree add"
    );
    assert!(
        reserved,
        "the destination was not reserved during worktree add"
    );
    assert_eq!(PathBuf::from(&session.worktree_path), destination);
    assert_ne!(second_path, destination);
    assert!(second_path.join(".git").exists());
    assert!(destination.join(".git").exists());
    assert_eq!(session.branch, "agent/same-task");
    // Released once registered.
    assert!(!crate::start_reservation::path_is_reserved(
        repo.path(),
        &destination
    ));
    assert!(!crate::start_reservation::slug_is_reserved(
        repo.path(),
        "same-task"
    ));
}

#[test]
fn a_failed_worktree_add_leaves_no_row_directory_branch_or_reservation() {
    let repo = repository();
    let storage = crate::test_storage::TestStorage::new();
    let mut broker = storage.open(repo.path()).unwrap();
    let repo_path = repo.path().to_path_buf();
    let seen: Rc<RefCell<Option<PathBuf>>> = Rc::default();
    let seen_in_hook = Rc::clone(&seen);
    crate::start_reservation::set_before_add_hook(Box::new(
        move |destination: &Path, branch: &str| {
            // Make `git worktree add -b` refuse: the branch already exists.
            git(&repo_path, &["branch", branch, "HEAD"]);
            *seen_in_hook.borrow_mut() = Some(destination.to_path_buf());
        },
    ));

    broker
        .start_worktree("doomed", None)
        .expect_err("worktree add must fail");
    let destination = seen.borrow_mut().take().expect("the hook ran");
    assert!(!destination.exists(), "reserved directory left behind");
    assert!(!branch_exists(repo.path(), "agent/doomed"));
    assert!(
        broker
            .store()
            .session_for_worktree(&destination.to_string_lossy())
            .unwrap()
            .is_none()
    );
    assert!(!crate::start_reservation::slug_is_reserved(
        repo.path(),
        "doomed"
    ));
    // The name is reusable.
    let session = broker.start_worktree("doomed", None).unwrap();
    assert_eq!(PathBuf::from(session.worktree_path), destination);
}

#[test]
fn a_killed_start_is_reaped_and_its_name_is_reusable() {
    let repo = repository();
    let storage = crate::test_storage::TestStorage::new();
    let mut broker = storage.open(repo.path()).unwrap();
    let first = broker.start_worktree("first", None).unwrap();
    let pool = Path::new(&first.worktree_path)
        .parent()
        .unwrap()
        .to_path_buf();
    let base = git(repo.path(), &["rev-parse", "HEAD"]);

    // A start killed after reserving and after `-b` created the branch.
    let destination = pool.join("killed");
    std::fs::create_dir(&destination).unwrap();
    git(repo.path(), &["branch", "agent/killed", &base]);
    crate::start_reservation::write_dead_reservation(
        repo.path(),
        "killed",
        &destination,
        "agent/killed",
        &base,
    );

    let session = broker.start_worktree("killed", None).unwrap();
    assert_eq!(PathBuf::from(&session.worktree_path), destination);
    assert_eq!(session.branch, "agent/killed");
    assert!(destination.join(".git").exists());
}

#[test]
fn a_dead_start_never_loses_a_checkout_it_completed() {
    let repo = repository();
    let storage = crate::test_storage::TestStorage::new();
    let mut broker = storage.open(repo.path()).unwrap();
    let session = broker.start_worktree("kept", None).unwrap();
    let destination = PathBuf::from(&session.worktree_path);
    let base = git(repo.path(), &["rev-parse", "HEAD"]);
    std::fs::write(destination.join("work.txt"), "keep me\n").unwrap();
    crate::start_reservation::write_dead_reservation(
        repo.path(),
        "kept",
        &destination,
        "agent/kept",
        &base,
    );

    let other = broker.start_worktree("other", None).unwrap();
    assert_ne!(other.worktree_path, session.worktree_path);
    assert!(destination.join("work.txt").is_file());
    assert!(branch_exists(repo.path(), "agent/kept"));
    assert!(!crate::start_reservation::slug_is_reserved(
        repo.path(),
        "kept"
    ));
}

#[test]
fn a_path_a_cleaned_session_recorded_is_never_handed_out_again() {
    let repo = repository();
    let storage = crate::test_storage::TestStorage::new();
    let mut broker = storage.open(repo.path()).unwrap();
    let old = broker.start_worktree("reused name", None).unwrap();
    let old_path = PathBuf::from(&old.worktree_path);
    // The old checkout and branch are gone; only the cleaned row remains.
    git(
        repo.path(),
        &["worktree", "remove", "--force", &old.worktree_path],
    );
    git(repo.path(), &["branch", "-D", &old.branch]);
    broker
        .store()
        .set_session_status(old.id, crate::types::SessionStatus::Cleaned, None)
        .unwrap();
    assert!(!old_path.exists());

    let new = broker.start_worktree("reused name", None).unwrap();
    assert_ne!(PathBuf::from(&new.worktree_path), old_path);
    assert_eq!(new.branch, "agent/reused-name-2");
}

#[test]
fn every_removal_lane_refuses_a_checkout_another_start_reserved() {
    let repo = repository();
    let storage = crate::test_storage::TestStorage::new();
    let mut broker = storage.open(repo.path()).unwrap();
    let session = broker.start_worktree("racing", None).unwrap();
    let path = PathBuf::from(&session.worktree_path);
    let base = git(repo.path(), &["rev-parse", "HEAD"]);
    let held = crate::start_reservation::hold_foreign_reservation(
        repo.path(),
        "racing",
        &path,
        "agent/racing",
        &base,
    );
    let main = crate::git::GitRepo::discover(repo.path()).unwrap();

    for error in [
        main.worktree_remove(&path, true).unwrap_err().to_string(),
        main.worktree_remove_registered(&path, true)
            .unwrap_err()
            .to_string(),
    ] {
        assert!(error.contains("start_in_progress"), "{error}");
    }
    let common = main.git_common_dir().unwrap();
    let error = crate::checkout_trash::move_worktree_to_trash(
        &main,
        &common,
        path.parent().unwrap(),
        &path,
        session.id,
        &mut |_, _, _| panic!("nothing is recorded for a reserved checkout"),
    )
    .unwrap_err();
    assert!(error.contains("start_in_progress"), "{error}");
    assert!(path.join(".git").exists());

    drop(held);
    main.worktree_remove(&path, true).unwrap();
    assert!(!path.exists());
}
