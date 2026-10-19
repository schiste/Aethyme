use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const CHILD_FLAG: &str = "AETHYME_TEST_FIXTURE_HOST_ISOLATION_CHILD";
const LIBRARY_CHILD_FLAG: &str = "AETHYME_TEST_FIXTURE_LIBRARY_PLACEMENT_CHILD";
const LIBRARY_PATH_REPORT: &str = "AETHYME_TEST_FIXTURE_LIBRARY_PATH_REPORT";

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

fn fixture() -> common::FixtureRepository {
    let repo = common::FixtureRepository::new();
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

fn configured_env(command: &Command) -> HashMap<OsString, Option<OsString>> {
    command
        .get_envs()
        .map(|(key, value)| (key.to_os_string(), value.map(OsStr::to_os_string)))
        .collect()
}

fn fixture_worktree_registry_paths(host_state: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(host_state)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("worktree-roots-")
        })
        .map(|entry| entry.path().join("registry.sqlite3"))
        .filter(|path| path.is_file())
        .collect()
}

#[test]
fn base_cli_helper_removes_inherited_preparation_cache() {
    let command = common::broker_cli(CLI, &["status"]);
    assert_eq!(
        configured_env(&command).get(OsStr::new("AETHYME_PREPARATION_CACHE_ROOT")),
        Some(&None),
        "the unconfigured CLI helper must remove the inherited cache"
    );
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

        let poison_cache = poison_root.join("preparation-cache");
        assert!(std::fs::read_dir(&poison_cache).unwrap().next().is_none());
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
        let fixture_preparation_cache = common::fixture_preparation_cache_root(repo.path())
            .canonicalize()
            .unwrap();

        assert_eq!(
            env.get(OsStr::new("AETHYME_HOST_STATE_DIR")),
            Some(&Some(fixture_state.clone().into_os_string()))
        );
        assert_eq!(
            env.get(OsStr::new("AETHYME_HOST_CACHE_DIR")),
            Some(&Some(fixture_cache.clone().into_os_string()))
        );
        assert_eq!(
            env.get(OsStr::new("AETHYME_PREPARATION_CACHE_ROOT")),
            Some(&Some(fixture_preparation_cache.into_os_string()))
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
        let registries = fixture_worktree_registry_paths(&fixture_state);
        assert_eq!(
            registries.len(),
            1,
            "expected one worktree registry beneath fixture host state {}",
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
        assert!(
            std::fs::read_dir(&poison_cache).unwrap().next().is_none(),
            "fixture must not initialize the inherited preparation cache"
        );
        return;
    }

    let poison_root = tempfile::tempdir().unwrap();
    std::fs::create_dir(poison_root.path().join("preparation-cache")).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "fixture_cli_replaces_poisoned_inherited_host_paths",
            "--nocapture",
        ])
        .env(CHILD_FLAG, "1")
        .env("AETHYME_TEST_POISON_ROOT", poison_root.path())
        .env(
            "AETHYME_PREPARATION_CACHE_ROOT",
            poison_root.path().join("preparation-cache"),
        )
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

#[test]
fn fixture_library_start_survives_broker_reopen_until_fixture_drop() {
    if std::env::var_os(LIBRARY_CHILD_FLAG).is_some() {
        let poison_root = PathBuf::from(std::env::var_os("AETHYME_TEST_POISON_ROOT").unwrap());
        let poison_cache = poison_root.join("preparation-cache");
        assert!(std::fs::read_dir(&poison_cache).unwrap().next().is_none());
        let repo = fixture();
        let mut broker = common::open_fixture_repository_broker(&repo).unwrap();
        let owned_root_path = broker.worktree_root().to_path_buf();
        let owned_cache = common::fixture_preparation_cache_root(repo.owner_path())
            .canonicalize()
            .unwrap();
        let mut child_context = Command::new("not-executed");
        broker
            .configure_storage_context_for_child(&mut child_context)
            .unwrap();
        assert_eq!(
            configured_env(&child_context).get(OsStr::new("AETHYME_PREPARATION_CACHE_ROOT")),
            Some(&Some(owned_cache.clone().into_os_string()))
        );
        let session = broker.start_worktree("library fixture", None).unwrap();
        let owned_root = owned_root_path.canonicalize().unwrap();
        let checkout = PathBuf::from(session.worktree_path);
        assert!(
            checkout.starts_with(&owned_root),
            "{} escaped fixture-owned root {}",
            checkout.display(),
            owned_root.display()
        );
        for path in [
            poison_root.join("host-state"),
            poison_root.join("host-cache"),
            poison_root.join("worktrees"),
        ] {
            assert!(
                !path.exists(),
                "library start used poisoned inherited path {}",
                path.display()
            );
        }
        drop(broker);
        assert!(
            checkout.exists(),
            "dropping one broker handle must not remove a fixture-owned checkout"
        );
        assert!(
            owned_root.exists(),
            "dropping one broker handle must not remove its fixture-owned root"
        );

        let reopened = common::open_fixture_repository_broker(&repo).unwrap();
        assert_eq!(reopened.worktree_root().canonicalize().unwrap(), owned_root);
        assert!(checkout.exists(), "reopening removed the existing checkout");
        drop(reopened);
        assert!(
            checkout.exists(),
            "the reopened broker removed the checkout"
        );

        std::fs::write(
            std::env::var_os(LIBRARY_PATH_REPORT).unwrap(),
            format!("{}\n{}\n", owned_root.display(), checkout.display()),
        )
        .unwrap();
        drop(repo);
        assert!(!checkout.exists(), "fixture teardown retained its checkout");
        assert!(!owned_cache.exists(), "fixture teardown retained its cache");
        assert!(
            !owned_root.exists(),
            "fixture teardown retained its worktree root"
        );
        assert!(
            std::fs::read_dir(&poison_cache).unwrap().next().is_none(),
            "fixture must not initialize the inherited preparation cache"
        );
        return;
    }

    let poison_root = tempfile::tempdir().unwrap();
    std::fs::create_dir(poison_root.path().join("preparation-cache")).unwrap();
    let report = poison_root.path().join("paths.txt");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "fixture_library_start_survives_broker_reopen_until_fixture_drop",
            "--nocapture",
        ])
        .env(LIBRARY_CHILD_FLAG, "1")
        .env("AETHYME_TEST_POISON_ROOT", poison_root.path())
        .env(
            "AETHYME_PREPARATION_CACHE_ROOT",
            poison_root.path().join("preparation-cache"),
        )
        .env("AETHYME_TEST_FIXTURE_LIBRARY_PATH_REPORT", &report)
        .env(
            "AETHYME_HOST_STATE_DIR",
            poison_root.path().join("host-state"),
        )
        .env(
            "AETHYME_HOST_CACHE_DIR",
            poison_root.path().join("host-cache"),
        )
        .env(
            "AETHYME_WORKTREE_ROOT",
            poison_root.path().join("worktrees"),
        )
        // Broker::open reads these before the fixture builder can override
        // anything. Keep this test scoped to host paths and worktree placement.
        .env_remove("AETHYME_BROKER_DB")
        .env_remove("AETHYME_GATE_BROKER_DATABASES")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child stdout={}\nchild stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let reported_paths = std::fs::read_to_string(report).unwrap();
    let mut paths = reported_paths.lines().map(PathBuf::from);
    let owned_root = paths.next().expect("owned root report");
    let checkout = paths.next().expect("checkout report");
    assert!(paths.next().is_none(), "unexpected child report fields");
    assert!(checkout.starts_with(&owned_root));
    assert!(
        !checkout.exists(),
        "child checkout survived its owner guard"
    );
    assert!(!owned_root.exists(), "child root survived its owner guard");
    for path in [
        poison_root.path().join("host-state"),
        poison_root.path().join("host-cache"),
        poison_root.path().join("worktrees"),
    ] {
        assert!(
            !path.exists(),
            "child used poisoned path {}",
            path.display()
        );
    }
}
