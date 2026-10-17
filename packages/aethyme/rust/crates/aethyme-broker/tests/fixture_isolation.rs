//! Retained session checkouts must disappear with their fixture owner.

#[path = "common/isolation.rs"]
mod isolation;

use std::path::PathBuf;
use std::process::Command;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const REPORT_ENV: &str = "AETHYME_TEST_FIXTURE_ISOLATION_REPORT";

#[test]
fn retained_worktree_uses_its_fixture_root_and_is_removed_at_teardown() {
    if let Some(report) = std::env::var_os(REPORT_ENV) {
        let poison = PathBuf::from(std::env::var_os("AETHYME_WORKTREE_ROOT").unwrap());
        let fixture = isolation::FixtureRepo::new();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["add", "README.md", ".gitignore"],
            vec!["commit", "-qm", "fixture"],
        ] {
            if args[0] == "add" {
                std::fs::write(fixture.path().join("README.md"), "fixture\n").unwrap();
                std::fs::write(fixture.path().join(".gitignore"), "/.aethyme/\n").unwrap();
            }
            let output = Command::new("git")
                .args(args)
                .current_dir(fixture.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }

        let expected_root = isolation::worktree_root(fixture.path());
        std::fs::create_dir_all(&expected_root).unwrap();
        let expected_root = expected_root.canonicalize().unwrap();
        let mut broker = isolation::open(fixture.path());
        let session = broker.start_worktree("retained fixture", None).unwrap();
        let worktree = PathBuf::from(session.worktree_path);
        assert!(
            worktree.canonicalize().unwrap().starts_with(&expected_root),
            "library checkout must stay under the fixture-owned worktree root"
        );
        assert!(!poison.exists(), "inherited placement must remain unused");

        let root = fixture.path().parent().unwrap();
        let mut command = isolation::command(
            CLI,
            fixture.path(),
            &[
                "start",
                "--task",
                "retained CLI fixture",
                "--short-name",
                "retained CLI fixture",
                "--json",
            ],
        );
        for (name, directory) in [
            ("AETHYME_WORKTREE_ROOT", "worktrees"),
            ("AETHYME_HOST_STATE_DIR", "host-state"),
            ("AETHYME_HOST_CACHE_DIR", "host-cache"),
        ] {
            let expected = root.join(directory);
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| { key == name && value == Some(expected.as_os_str()) })
            );
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let cli_session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let cli_worktree = PathBuf::from(cli_session["worktree_path"].as_str().unwrap());
        assert!(
            cli_worktree
                .canonicalize()
                .unwrap()
                .starts_with(&expected_root),
            "CLI checkout must stay under the fixture-owned worktree root"
        );
        let repo = fixture.path().to_path_buf();
        let root = repo.parent().unwrap().to_path_buf();
        drop(broker);
        drop(fixture);
        assert!(
            !root.exists(),
            "all fixture data must be removed at teardown"
        );
        assert!(
            !worktree.exists(),
            "retained checkout must share that owner"
        );
        assert!(!cli_worktree.exists(), "CLI checkout must share that owner");
        std::fs::write(
            report,
            serde_json::to_vec(&serde_json::json!({
                "root": root,
                "worktree": worktree,
                "cli_worktree": cli_worktree,
            }))
            .unwrap(),
        )
        .unwrap();
        return;
    }

    let owner = tempfile::tempdir().unwrap();
    let report = owner.path().join("report.json");
    let poison = owner.path().join("poison-worktrees");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "retained_worktree_uses_its_fixture_root_and_is_removed_at_teardown",
            "--nocapture",
        ])
        .env(REPORT_ENV, &report)
        .env("AETHYME_WORKTREE_ROOT", &poison)
        .env(
            "AETHYME_HOST_STATE_DIR",
            owner.path().join("startup-host-state"),
        )
        .env(
            "AETHYME_HOST_CACHE_DIR",
            owner.path().join("startup-host-cache"),
        )
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    assert!(!PathBuf::from(report["root"].as_str().unwrap()).exists());
    assert!(!PathBuf::from(report["worktree"].as_str().unwrap()).exists());
    assert!(!PathBuf::from(report["cli_worktree"].as_str().unwrap()).exists());
    assert!(!poison.exists());
}
