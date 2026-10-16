use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const CHILD_FLAG: &str = "AETHYME_TEST_FIXTURE_HOST_ISOLATION_CHILD";

mod common;

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.path().join(".gitignore"),
        "/.aethyme/*\n!/.aethyme/gates.toml\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("README.md"), "fixture\n").unwrap();
    std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
    std::fs::write(
        repo.path().join(".aethyme/gates.toml"),
        r#"[[gate]]
name = "host-isolation"
command = "printf cache > \"$AETHYME_GATE_CACHE_DIR/cache-marker\""
cache = false
resource_wait_seconds = 1

[gate.managed_cache]
key = "host-isolation"
max_bytes = 64
"#,
    )
    .unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "fixture"]);
    repo
}

fn merged(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn configured_env(command: &Command) -> HashMap<OsString, Option<OsString>> {
    command
        .get_envs()
        .map(|(key, value)| (key.to_os_string(), value.map(OsStr::to_os_string)))
        .collect()
}

#[test]
fn fixture_cli_replaces_poisoned_inherited_host_paths() {
    if std::env::var_os(CHILD_FLAG).is_some() {
        let poison_root = PathBuf::from(std::env::var_os("AETHYME_TEST_POISON_ROOT").unwrap());
        for (name, path) in [
            ("AETHYME_HOST_STATE_DIR", poison_root.join("host-state")),
            ("AETHYME_HOST_CACHE_DIR", poison_root.join("host-cache")),
            ("AETHYME_BROKER_DB", poison_root.join("broker.db")),
            ("AETHYME_WORKTREE_ROOT", poison_root.join("worktrees")),
        ] {
            assert_eq!(
                std::env::var_os(name).as_deref(),
                Some(path.as_os_str()),
                "{name}"
            );
        }
        assert_eq!(
            std::env::var("AETHYME_GATE_BROKER_DATABASES").unwrap(),
            "poisoned-gate-database-scope"
        );

        let repo = fixture();
        let unrelated = fixture();
        let mut command = common::broker_cli_for_fixture(
            CLI,
            &["start", "--task", "host isolation", "--json"],
            repo.path(),
        );
        command.current_dir(repo.path());
        let env = configured_env(&command);
        let fixture_state = common::fixture_host_state_dir(repo.path());
        let fixture_cache = common::fixture_host_cache_dir(repo.path());

        assert_eq!(
            env.get(OsStr::new("AETHYME_HOST_STATE_DIR")),
            Some(&Some(fixture_state.clone().into_os_string()))
        );
        assert_eq!(
            env.get(OsStr::new("AETHYME_HOST_CACHE_DIR")),
            Some(&Some(fixture_cache.clone().into_os_string()))
        );
        for removed in [
            "AETHYME_BROKER_DB",
            "AETHYME_GATE_BROKER_DATABASES",
            "AETHYME_WORKTREE_ROOT",
        ] {
            assert_eq!(env.get(OsStr::new(removed)), Some(&None), "{removed}");
        }

        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            fixture_state.join("host-operations.db").is_file(),
            "fixture host operations were not written under {}",
            fixture_state.display()
        );
        assert!(
            repo.path().join(".aethyme/broker.db").is_file(),
            "fixture broker state was not written in the repository"
        );
        let gate = common::broker_cli_for_fixture(
            CLI,
            &["advanced", "gates", "run", "--all", "--json"],
            repo.path(),
        )
        .current_dir(repo.path())
        .output()
        .unwrap();
        assert!(
            gate.status.success(),
            "gate stdout={}\ngate stderr={}",
            String::from_utf8_lossy(&gate.stdout),
            String::from_utf8_lossy(&gate.stderr)
        );
        let repositories: Vec<_> = std::fs::read_dir(fixture_cache.join("gates"))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(repositories.len(), 1, "unexpected fixture cache entries");
        assert!(
            repositories[0]
                .path()
                .join("host-isolation/cache-marker")
                .is_file(),
            "managed gate cache was not written below {}",
            fixture_cache.display()
        );
        assert!(
            !common::fixture_host_state_dir(unrelated.path()).exists(),
            "an unrelated fixture received the first fixture's host state"
        );
        assert!(
            !common::fixture_host_cache_dir(unrelated.path()).exists(),
            "an unrelated fixture received the first fixture's host cache"
        );
        assert!(
            !unrelated.path().join(".aethyme/broker.db").exists(),
            "an unrelated fixture received broker state"
        );
        for path in [
            poison_root.join("host-state"),
            poison_root.join("host-cache"),
            poison_root.join("broker.db"),
            poison_root.join("worktrees"),
        ] {
            assert!(
                !path.exists(),
                "poisoned inherited path was used: {}",
                path.display()
            );
        }
        return;
    }

    let poison_root = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "fixture_cli_replaces_poisoned_inherited_host_paths",
            "--nocapture",
        ])
        .env(CHILD_FLAG, "1")
        .env("AETHYME_TEST_POISON_ROOT", poison_root.path())
        .env(
            "AETHYME_HOST_STATE_DIR",
            poison_root.path().join("host-state"),
        )
        .env(
            "AETHYME_HOST_CACHE_DIR",
            poison_root.path().join("host-cache"),
        )
        .env("AETHYME_BROKER_DB", poison_root.path().join("broker.db"))
        .env(
            "AETHYME_GATE_BROKER_DATABASES",
            "poisoned-gate-database-scope",
        )
        .env(
            "AETHYME_WORKTREE_ROOT",
            poison_root.path().join("worktrees"),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child stdout={}\nchild stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
