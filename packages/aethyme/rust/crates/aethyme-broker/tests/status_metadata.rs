//! Routine status observes coordination records without deriving Git remedies.
use aethyme_broker::{Broker, BrokerOpenOptions, SessionStatus};
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Fixture {
    _owner: tempfile::TempDir,
    broker: Broker,
    worktree: PathBuf,
    operation_db: PathBuf,
    id: i64,
}
impl Fixture {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "initial"]);
        let worktree = root.join("worktree");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "work",
                worktree.to_str().unwrap(),
            ],
        );
        let operation_db = root.join("host-state/host-operations.db");
        let options = BrokerOpenOptions::default()
            .with_host_state_dir(root.join("host-state"))
            .with_host_cache_dir(root.join("host-cache"))
            .with_host_operation_database(&operation_db)
            .with_worktree_root(root.join("managed-worktrees"))
            .with_collaboration_state(root.join("collaboration"));
        let mut broker = Broker::open_with_options(&repo, options).unwrap();
        let id = broker.adopt(&worktree, None).unwrap().id;
        broker.claim_lease(id, "README.md", None).unwrap();
        broker
            .store()
            .set_session_status(id, SessionStatus::Stale, None)
            .unwrap();
        Self {
            _owner: owner,
            broker,
            worktree,
            operation_db,
            id,
        }
    }
}

#[test]
fn brief_defers_git_remedies_but_full_blockers_preserves_them() {
    let fixture = Fixture::new();
    std::fs::write(fixture.worktree.join("README.md"), "unrepresented change\n").unwrap();
    git(&fixture.worktree, &["add", "README.md"]);
    git(&fixture.worktree, &["commit", "-qm", "undelivered"]);
    let full = fixture.broker.blockers();
    let lease = full
        .blockers
        .iter()
        .find(|item| item.session_id == Some(fixture.id) && item.id.starts_with("lease:"))
        .unwrap();
    assert!(
        lease.cause.contains("committed change not yet delivered"),
        "{lease:?}"
    );
    assert_eq!(
        lease.clear,
        format!("aethyme broker submit --session {}", fixture.id)
    );
    let brief = fixture.broker.status_brief_snapshot(0).unwrap();
    let item = brief
        .advice
        .iter()
        .find(|item| item.id == "blockers.present")
        .unwrap();
    assert!(item.summary.starts_with("1 blocker"), "{item:?}");
    assert_eq!(item.commands, ["aethyme broker unblock"]);
    assert!(item.reason.contains("not been inspected"));
    assert!(item.evidence.iter().any(|line| line.starts_with(&lease.id)));
    assert!(
        item.evidence
            .iter()
            .all(|line| !line.contains("committed change not yet delivered")),
        "{item:?}"
    );
}

#[test]
fn brief_reports_unavailable_blocker_sources() {
    let fixture = Fixture::new();
    if fixture.operation_db.exists() {
        std::fs::remove_file(&fixture.operation_db).unwrap();
    }
    std::fs::create_dir(&fixture.operation_db).unwrap();
    let brief = fixture.broker.status_brief_snapshot(0).unwrap();
    let item = brief
        .advice
        .iter()
        .find(|item| item.id == "blockers.sources-unavailable")
        .expect("unreadable sources must not look clean");
    assert!(item.reason.contains("incomplete"));
    assert!(
        item.evidence
            .iter()
            .any(|line| line.starts_with("host-operations.db:")),
        "{item:?}"
    );
    assert_eq!(item.commands, ["aethyme broker unblock"]);
}
