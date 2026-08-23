use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{
    Broker, IntegrationReconcileCommitOrigin, IntegrationReconcileEquivalence,
    IntegrationReconcileOptions, MergeStatus,
};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
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
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn commit(root: &Path, path: &str, content: &str, message: &str) -> String {
    std::fs::write(root.join(path), content).unwrap();
    git(root, &["add", path]);
    git(root, &["commit", "-qm", message]);
    git(root, &["rev-parse", "HEAD"])
}

struct DeployDivergenceFixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    old_integration: String,
    upstream_head: String,
    promoted_commit: String,
    functional_upstream: String,
    unrecorded_commit: String,
    pending_head: String,
    pending_entry_id: i64,
}

impl DeployDivergenceFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let remote = tmp.path().join("remote.git");
        let deploy = tmp.path().join("deploy");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::write(repo.join(".gitignore"), ".aethyme/\n").unwrap();
        std::fs::write(repo.join("src/service.txt"), "feature=off\n").unwrap();
        std::fs::write(repo.join("docs/CHANGELOG.md"), "Release 0\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "initial"]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&repo, &["push", "-qu", "origin", "main"]);

        let mut broker = Broker::open(&repo).unwrap();
        let session = broker.start_worktree("functional promotion").unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        commit(
            &worktree,
            "src/service.txt",
            "feature=on\n",
            "enable feature",
        );
        let promoted = broker.submit(session.id).unwrap();
        assert!(promoted.promoted);
        let promoted_commit = serde_json::from_str::<serde_json::Value>(
            promoted.entry.details_json.as_deref().unwrap(),
        )
        .unwrap()["commit"]
            .as_str()
            .unwrap()
            .to_string();

        git(&repo, &["switch", "aethyme/integration"]);
        let unrecorded_commit = commit(
            &repo,
            "docs/CHANGELOG.md",
            "Release candidate\n",
            "manually resolve release notes",
        );
        let old_integration = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["switch", "main"]);

        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                deploy.to_str().unwrap(),
            ],
        );
        let functional_upstream = commit(
            &deploy,
            "src/service.txt",
            "feature=on\n",
            "deploy equivalent feature",
        );
        commit(
            &deploy,
            "docs/CHANGELOG.md",
            "Release 1\n",
            "write release notes during deploy",
        );
        git(&deploy, &["push", "-q", "origin", "main"]);
        git(&repo, &["fetch", "-q", "origin", "main"]);
        let upstream_head = git(&repo, &["rev-parse", "origin/main"]);

        std::fs::write(
            repo.join(".aethyme/config.toml"),
            "[promote]\nmode = \"manual\"\n",
        )
        .unwrap();
        let pending = broker.start_worktree("pending queue work").unwrap();
        let pending_worktree = PathBuf::from(&pending.worktree_path);
        let pending_head = commit(
            &pending_worktree,
            "src/pending.txt",
            "pending=true\n",
            "pending queue change",
        );
        let pending_outcome = broker.submit(pending.id).unwrap();
        assert_eq!(pending_outcome.entry.status, MergeStatus::Verified);

        Self {
            _tmp: tmp,
            repo,
            old_integration,
            upstream_head,
            promoted_commit,
            functional_upstream,
            unrecorded_commit,
            pending_head,
            pending_entry_id: pending_outcome.entry.id,
        }
    }
}

#[test]
fn reconciliation_plan_classifies_every_relevant_commit_with_full_provenance() {
    let fixture = DeployDivergenceFixture::new();
    let mut broker = Broker::open(&fixture.repo).unwrap();
    let report = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: None,
        })
        .unwrap();

    let commits = &report.plan.commits;
    assert_eq!(report.plan.common_base.len(), 40);
    assert_eq!(
        commits
            .iter()
            .filter(|commit| {
                commit.origin == IntegrationReconcileCommitOrigin::UpstreamOnlyExternalWork
            })
            .count(),
        2
    );
    assert_eq!(
        commits
            .iter()
            .filter(|commit| {
                commit.origin == IntegrationReconcileCommitOrigin::RecordedPromotedWork
            })
            .count(),
        1
    );
    assert_eq!(
        commits
            .iter()
            .filter(|commit| {
                commit.origin == IntegrationReconcileCommitOrigin::UnrecordedIntegrationCommit
            })
            .count(),
        1
    );
    assert_eq!(
        commits
            .iter()
            .filter(|commit| {
                commit.origin == IntegrationReconcileCommitOrigin::PendingQueueEntry
            })
            .count(),
        1
    );

    let promoted = commits
        .iter()
        .find(|commit| commit.commit == fixture.promoted_commit)
        .unwrap();
    assert_eq!(
        promoted.equivalence,
        IntegrationReconcileEquivalence::PatchEquivalent
    );
    assert_eq!(
        promoted.matching_commits,
        vec![fixture.functional_upstream.clone()]
    );
    let upstream_copy = commits
        .iter()
        .find(|commit| commit.commit == fixture.functional_upstream)
        .unwrap();
    assert_eq!(
        upstream_copy.equivalence,
        IntegrationReconcileEquivalence::PatchEquivalent
    );
    assert_eq!(
        upstream_copy.matching_commits,
        vec![fixture.promoted_commit.clone()]
    );

    let unrecorded = commits
        .iter()
        .find(|commit| commit.commit == fixture.unrecorded_commit)
        .unwrap();
    assert_eq!(
        unrecorded.equivalence,
        IntegrationReconcileEquivalence::None
    );
    assert_eq!(unrecorded.files, vec!["docs/CHANGELOG.md"]);

    let pending = commits
        .iter()
        .find(|commit| commit.commit == fixture.pending_head)
        .unwrap();
    assert_eq!(pending.queue_entry_id, Some(fixture.pending_entry_id));
    assert_eq!(pending.queue_status, Some(MergeStatus::Verified));
    assert_eq!(pending.equivalence, IntegrationReconcileEquivalence::None);
    assert!(commits.iter().all(|commit| commit.commit.len() == 40));
    let json = serde_json::to_value(&report.plan).unwrap();
    assert_eq!(
        json["commits"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|commit| commit["origin"] == "pending_queue_entry")
            .count(),
        1
    );
}

#[test]
fn deploy_written_main_divergence_is_blocked_by_unrecorded_integration_work() {
    let fixture = DeployDivergenceFixture::new();
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "diff",
                "--name-only",
                "origin/main",
                "aethyme/integration",
                "--",
                "src/service.txt",
            ],
        ),
        "",
        "functional work is equivalent across the dual identities"
    );
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "diff",
                "--name-only",
                "origin/main",
                "aethyme/integration",
                "--",
                "docs/CHANGELOG.md",
            ],
        ),
        "docs/CHANGELOG.md",
        "deploy-written release notes are the genuine content divergence"
    );

    let mut broker = Broker::open(&fixture.repo).unwrap();
    let report = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: None,
        })
        .unwrap();

    assert!(!report.safe);
    assert!(!report.applied);
    assert!(report.entries.is_empty());
    assert!(report.warnings.iter().any(|warning| {
        warning.contains("unrecorded work")
            || warning.contains("not a contiguous promoted queue layer")
    }));
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "aethyme/integration"]),
        fixture.old_integration
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "origin/main"]),
        fixture.upstream_head
    );
}
