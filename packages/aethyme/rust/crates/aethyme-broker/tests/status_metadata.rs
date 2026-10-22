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
fn stale_lease_and_undelivered_commit_remain_visible_without_global_blocker_advice() {
    let fixture = Fixture::new();
    std::fs::write(fixture.worktree.join("README.md"), "unrepresented change\n").unwrap();
    git(&fixture.worktree, &["add", "README.md"]);
    git(&fixture.worktree, &["commit", "-qm", "undelivered"]);

    let session = fixture.broker.store().session(fixture.id).unwrap();
    assert_eq!(session.status, SessionStatus::Stale);
    assert!(Path::new(&session.worktree_path).exists());
    let leases = fixture.broker.store().session_leases(fixture.id).unwrap();
    assert!(
        leases.iter().any(|lease| lease.path == "README.md"),
        "the stale session's explicit lease remains visible: {leases:?}"
    );

    let full = fixture.broker.blockers();
    assert!(
        full.blockers
            .iter()
            .all(|item| item.kind != aethyme_broker::BlockerKind::PathLease),
        "a claim-specific stale lease is not a repository-wide blocker: {full:?}"
    );

    let brief = fixture.broker.status_brief_snapshot(0).unwrap();
    assert!(
        brief
            .advice
            .iter()
            .all(|item| item.id != "blockers.present"),
        "routine status must not invent blocker advice from a stale path claim: {brief:?}"
    );
}

#[test]
fn brief_reports_unavailable_blocker_sources() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.operation_db.parent().unwrap()).unwrap();
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
