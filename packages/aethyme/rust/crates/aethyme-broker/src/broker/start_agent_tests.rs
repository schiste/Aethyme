use super::{Broker, lifecycle};
use crate::git::GitRepo;
use std::path::Path;
use std::process::Command;

fn run_git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .status()
        .expect("run git fixture command");
    assert!(status.success(), "git {args:?} failed with {status}");
}

fn initialized_repo(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    run_git(root, &["init", "-q", "-b", "main"]);
    run_git(root, &["config", "user.name", "Broker start test"]);
    run_git(
        root,
        &["config", "user.email", "broker-start@example.invalid"],
    );
    run_git(root, &["commit", "--allow-empty", "-qm", "initial"]);
}

#[test]
fn failed_session_registration_never_releases_the_agent_command() {
    let temp = tempfile::tempdir().unwrap();
    let repo_root = temp.path().join("repo");
    let worktree_root = temp.path().join("external-worktrees");
    let host_state = temp.path().join("host-state");
    initialized_repo(&repo_root);
    let options = crate::BrokerOpenOptions::default()
        .with_host_state_dir(&host_state)
        .with_host_cache_dir(temp.path().join("host-cache"))
        .with_worktree_root(worktree_root.clone())
        .with_collaboration_state(host_state.clone())
        .with_host_operation_database(host_state.join("host-operations.db"));
    let mut broker = Broker::open_with_options(&repo_root, options).unwrap();
    broker
        .store()
        .connection_mut()
        .execute_batch(
            "CREATE TRIGGER reject_start_agent
             BEFORE INSERT ON sessions
             BEGIN SELECT RAISE(ABORT, 'injected registration failure'); END;",
        )
        .unwrap();

    let sentinel = temp.path().join("user-command-ran");
    let command = format!("printf ran > '{}'", sentinel.display());
    let result = broker.start_agent_report("blocked registration", &command, None);
    assert!(
        result.is_err(),
        "injected session insert unexpectedly succeeded"
    );

    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(
        !sentinel.exists(),
        "user command ran before the failed registration was known"
    );
    let session_count: i64 = broker
        .store()
        .connection_mut()
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(session_count, 0);

    let git = GitRepo::discover(&repo_root).unwrap();
    let expected_worktree = worktree_root.join("blocked-registration");
    assert!(
        !expected_worktree.exists(),
        "failed registration left worktree {}",
        expected_worktree.display()
    );
    assert!(
        git.resolve_ref("refs/heads/agent/blocked-registration")
            .is_none(),
        "failed registration left the agent branch"
    );
    assert!(git.worktree_paths().unwrap().is_empty());

    let log_dir = repo_root.join(".aethyme/logs");
    if log_dir.exists() {
        let log_count = std::fs::read_dir(log_dir).unwrap().count();
        #[cfg(unix)]
        assert_eq!(log_count, 0, "failed registration left its unowned log");
        #[cfg(not(unix))]
        assert_eq!(
            log_count, 1,
            "without stable file identity, rollback should preserve the log and report it"
        );
    }
}

#[test]
fn rollback_removal_preserves_an_unregistered_directory_after_git_refusal() {
    let temp = tempfile::tempdir().unwrap();
    let repo_root = temp.path().join("repo");
    initialized_repo(&repo_root);
    let unregistered = temp.path().join("uncertain-leftover");
    std::fs::create_dir_all(&unregistered).unwrap();
    let evidence = unregistered.join("keep-for-recovery.txt");
    std::fs::write(&evidence, b"unregistered worktree evidence").unwrap();

    let repo = GitRepo::discover(&repo_root).unwrap();
    let result = repo.worktree_remove_without_orphan_fallback(&unregistered);
    assert!(
        result.is_err(),
        "Git must refuse to remove an unregistered path"
    );
    assert_eq!(
        std::fs::read(&evidence).unwrap(),
        b"unregistered worktree evidence",
        "a failed rollback must retain uncertain path contents"
    );
}

#[test]
fn agent_log_allocation_never_truncates_an_existing_log() {
    let temp = tempfile::tempdir().unwrap();
    let old_log = temp.path().join("same-task.log");
    std::fs::write(&old_log, b"prior evidence").unwrap();

    let (new_log, _file) = lifecycle::create_agent_log(temp.path(), "same-task").unwrap();
    assert_eq!(
        new_log.path.file_name().unwrap().to_string_lossy(),
        "same-task-2.log"
    );
    assert_eq!(std::fs::read(old_log).unwrap(), b"prior evidence");
}

#[test]
fn rollback_preserves_a_replacement_at_the_created_agent_log_path() {
    let temp = tempfile::tempdir().unwrap();
    let repo_root = temp.path().join("repo");
    initialized_repo(&repo_root);
    let repo = GitRepo::discover(&repo_root).unwrap();
    let base = repo.resolve_ref("HEAD").unwrap();
    let worktree_path = temp.path().join("agent-worktree");
    let worktree_text = worktree_path.to_str().unwrap();
    let branch = "agent/log-replacement";
    run_git(
        &repo_root,
        &["worktree", "add", "-b", branch, worktree_text, &base],
    );

    let (created_log, created_file) = lifecycle::create_agent_log(temp.path(), "agent").unwrap();
    std::fs::remove_file(&created_log.path).unwrap();
    std::fs::write(&created_log.path, b"replacement evidence").unwrap();

    let recovery = lifecycle::rollback_unregistered_start(
        &repo,
        &worktree_path,
        branch,
        &base,
        Some(&created_log),
    );
    assert_eq!(
        recovery.len(),
        1,
        "replacement should be retained with a diagnostic"
    );
    assert!(
        recovery[0].contains("created by this start attempt"),
        "{recovery:?}"
    );
    assert!(
        !worktree_path.exists(),
        "the exact clean worktree should be removed"
    );
    assert!(repo.resolve_ref(&format!("refs/heads/{branch}")).is_none());
    assert_eq!(
        std::fs::read(&created_log.path).unwrap(),
        b"replacement evidence",
        "rollback must preserve a replacement file"
    );
    drop(created_file);
}

#[cfg(unix)]
#[test]
fn aborting_a_blocked_agent_signals_only_its_owned_process_group() {
    use std::os::unix::process::CommandExt as _;

    let temp = tempfile::tempdir().unwrap();
    let sentinel = temp.path().join("must-not-run");
    let command = format!("printf ran > '{}'", sentinel.display());
    let output = std::fs::File::create(temp.path().join("blocked.log")).unwrap();
    let error = output.try_clone().unwrap();
    let mut blocked = lifecycle::spawn_blocked_agent(&command, temp.path(), output, error).unwrap();

    let mut unrelated = Command::new("sh")
        .arg("-c")
        .arg("sleep 30")
        .current_dir(temp.path())
        .process_group(0)
        .spawn()
        .unwrap();
    let unrelated_pid = unrelated.id();

    let abort = blocked.abort();
    let unrelated_state = unrelated.try_wait();
    // SAFETY: this process group was created by this test and is distinct from
    // the blocked agent's group.
    unsafe {
        libc::killpg(unrelated_pid as i32, libc::SIGKILL);
    }
    let _ = unrelated.kill();
    let _ = unrelated.wait();

    assert!(abort.is_ok(), "{abort:?}");
    assert!(
        matches!(unrelated_state, Ok(None)),
        "abort signalled a process it did not own: {unrelated_state:?}"
    );
    assert!(!sentinel.exists(), "the gated user command was released");
}
