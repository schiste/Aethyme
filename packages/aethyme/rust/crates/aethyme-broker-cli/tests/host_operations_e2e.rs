use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use aethyme_broker::{
    GitRepo, HostOperationError, HostOperationGuard, OperationEffect, OperationProvider,
    OperationStatus, host_operation, reconcile_host_operation, resolve_remote_target,
};

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
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
}

struct CloneFixture {
    _tmp: tempfile::TempDir,
    clone_a: PathBuf,
    worktree_a: PathBuf,
    clone_b: PathBuf,
    database: PathBuf,
}

impl CloneFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let seed = tmp.path().join("seed");
        let clone_a = tmp.path().join("clone-a");
        let clone_b = tmp.path().join("clone-b");
        let worktree_a = tmp.path().join("worktree-a");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&seed).unwrap();
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("tracked.txt"), "base\n").unwrap();
        git(&seed, &["add", "tracked.txt"]);
        git(&seed, &["commit", "-qm", "init"]);
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-qu", "origin", "main"]);
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                clone_a.to_str().unwrap(),
            ],
        );
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                clone_b.to_str().unwrap(),
            ],
        );
        git(
            &clone_a,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "worker-a",
                worktree_a.to_str().unwrap(),
                "main",
            ],
        );
        Self {
            database: tmp.path().join("host-state/operations.db"),
            _tmp: tmp,
            clone_a,
            worktree_a,
            clone_b,
        }
    }

    fn keys(&self) -> (String, String, String) {
        let key = |path: &Path| {
            let repo = GitRepo::discover(path).unwrap();
            resolve_remote_target(&repo, "origin", None)
                .unwrap()
                .coordination_key
        };
        (
            key(&self.clone_a),
            key(&self.worktree_a),
            key(&self.clone_b),
        )
    }
}

#[test]
fn independent_clones_and_worktrees_share_one_remote_write_lock() {
    let fixture = CloneFixture::new();
    let (clone_a_key, worktree_key, clone_b_key) = fixture.keys();
    assert_eq!(clone_a_key, worktree_key);
    assert_eq!(clone_a_key, clone_b_key);

    let mut first = HostOperationGuard::begin(
        &fixture.database,
        &clone_a_key,
        OperationProvider::Git,
        OperationEffect::Write,
    )
    .unwrap();
    first.mark_running().unwrap();

    let database = fixture.database.clone();
    let (attempting_tx, attempting_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let second = std::thread::spawn(move || {
        attempting_tx.send(()).unwrap();
        let mut guard = HostOperationGuard::begin(
            &database,
            &clone_b_key,
            OperationProvider::Git,
            OperationEffect::Write,
        )
        .unwrap();
        acquired_tx.send(()).unwrap();
        guard.mark_running().unwrap();
        guard.finish(OperationStatus::Succeeded).unwrap();
    });

    attempting_rx.recv().unwrap();
    assert!(
        acquired_rx
            .recv_timeout(Duration::from_millis(150))
            .is_err()
    );
    first.finish(OperationStatus::Succeeded).unwrap();
    drop(first);
    acquired_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    second.join().unwrap();
}

#[test]
fn unknown_outcome_in_one_clone_blocks_another_until_explicit_reconciliation() {
    let fixture = CloneFixture::new();
    let (clone_a_key, _, clone_b_key) = fixture.keys();
    let mut first = HostOperationGuard::begin(
        &fixture.database,
        &clone_a_key,
        OperationProvider::Git,
        OperationEffect::Write,
    )
    .unwrap();
    let operation_id = first.operation().operation_id.clone();
    first.mark_running().unwrap();
    drop(first);

    let blocked = match HostOperationGuard::begin(
        &fixture.database,
        &clone_b_key,
        OperationProvider::Git,
        OperationEffect::Write,
    ) {
        Ok(_) => panic!("the unresolved operation should block the other clone"),
        Err(error) => error,
    };
    assert!(matches!(
        blocked,
        HostOperationError::Blocked {
            operation_id: blocked_id,
            remote_key,
        } if blocked_id == operation_id && remote_key == clone_a_key
    ));
    assert_eq!(
        host_operation(&fixture.database, &operation_id)
            .unwrap()
            .unwrap()
            .status,
        OperationStatus::OutcomeUnknown
    );

    reconcile_host_operation(&fixture.database, &operation_id, false).unwrap();
    assert!(
        HostOperationGuard::begin(
            &fixture.database,
            &clone_a_key,
            OperationProvider::Git,
            OperationEffect::Write,
        )
        .is_ok()
    );
}
