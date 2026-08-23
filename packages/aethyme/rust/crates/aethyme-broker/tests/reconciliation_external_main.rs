use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, IntegrationReconcileOptions};

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
        assert!(broker.submit(session.id).unwrap().promoted);

        git(&repo, &["switch", "aethyme/integration"]);
        commit(
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
        commit(
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

        Self {
            _tmp: tmp,
            repo,
            old_integration,
            upstream_head,
        }
    }
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
