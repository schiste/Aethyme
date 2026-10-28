//! T09/T35 for the collaboration state root (#656): run every legacy deleter
//! that can act on host storage with a collaboration root present, and
//! require its bytes to be untouched.
//!
//! The deleters are this binary's, unchanged by #656, so they are the same
//! code an older binary runs. Each case gets its own processes so the host
//! state, host cache and worktree container can be set per case.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::collaboration_state::{ProjectKey, open_for_repository};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const CHILD: &str = "AETHYME_TEST_COLLABORATION_OPEN";
mod common;

struct Host {
    state: tempfile::TempDir,
    cache: tempfile::TempDir,
    home: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl Host {
    fn new() -> Self {
        let host = Self {
            state: tempfile::tempdir().unwrap(),
            cache: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
        };
        let repo = host.repo.path();
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "init"]);
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(
            repo.join(".aethyme/broker.toml"),
            "[retention]\norphan_worktree_roots_days = 0\nartifact_reclaim_days = 0\n\
             artifact_sweep_budget_ms = 5000\n",
        )
        .unwrap();
        host
    }

    fn collaboration(&self) -> PathBuf {
        self.state.path().join("collaboration")
    }

    /// The environment every process in a case shares. `container` is
    /// `AETHYME_WORKTREE_ROOT`; `None` leaves the default `<state>/worktrees`.
    fn env(&self, command: &mut Command, container: Option<&Path>) {
        command
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env("AETHYME_HOST_STATE_DIR", self.state.path())
            .env("AETHYME_HOST_CACHE_DIR", self.cache.path())
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env_remove("AETHYME_WORKTREE_ROOT");
        if let Some(container) = container {
            command.env("AETHYME_WORKTREE_ROOT", container);
        }
    }

    fn cli(&self, container: Option<&Path>, args: &[&str]) -> Output {
        let mut command = common::broker_cli(CLI, args);
        self.env(&mut command, container);
        command.output().unwrap()
    }

    fn cli_from(&self, repository: &Path, container: Option<&Path>, args: &[&str]) -> Output {
        let mut command = common::broker_cli(CLI, args);
        self.env(&mut command, container);
        command.current_dir(repository);
        command.output().unwrap()
    }

    fn json(&self, container: Option<&Path>, args: &[&str]) -> serde_json::Value {
        let output = self.cli(container, args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Open the collaboration store in a child process with this case's
    /// environment: `Ok(receipt label)` or `Err(error code)`.
    fn open(&self, container: Option<&Path>) -> Result<String, String> {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "child_opens_the_collaboration_store",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        self.env(&mut command, container);
        let output = command.env(CHILD, "1").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout
            .lines()
            .find_map(|line| line.split_once("RESULT ").map(|(_, result)| result))
            .unwrap_or_else(|| panic!("child printed no result: {stdout}"));
        match line.split_once(' ') {
            Some(("ok", label)) => Ok(label.to_string()),
            Some(("err", code)) => Err(code.to_string()),
            _ => panic!("unexpected child result {line:?}"),
        }
    }

    /// Open the store and leave stand-ins for retained objects and an
    /// in-progress capture, as the archive and capture slices will.
    fn seed(&self) -> PathBuf {
        self.open(None).expect("the default layout opens");
        let project = self.collaboration().join("proj-t");
        for (path, bytes) in [
            ("objects/sha256/ab/abcdef", b"retained source\n".as_slice()),
            ("spool/op-1/blob", b"capture in progress\n"),
        ] {
            let path = project.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        project
    }
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
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

/// Every path under `root` with its bytes and mode.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
    use std::os::unix::fs::PermissionsExt;

    let mut entries = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            let bytes = if metadata.is_dir() {
                pending.push(path.clone());
                Vec::new()
            } else {
                std::fs::read(&path).unwrap()
            };
            entries.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                (bytes, metadata.permissions().mode()),
            );
        }
    }
    entries
}

/// A handwritten root marker without the host-origin fingerprint. It must
/// defer orphan removal rather than authorize deleting the marked directory.
fn orphan_root(container: &Path) -> PathBuf {
    let root = container.join("gone-0123456789abcdef");
    std::fs::create_dir_all(root.join("old-session")).unwrap();
    std::fs::write(root.join("old-session/leftover"), "bytes\n").unwrap();
    std::fs::write(
        root.join(".aethyme-worktree-root.json"),
        serde_json::json!({
            "schema_version": 1,
            "repository_key": "gone-0123456789abcdef",
            "repository_root": container.join("deleted-repository"),
        })
        .to_string(),
    )
    .unwrap();
    root
}

/// Obtain a genuine host-registered orphan: admit a real repository, then
/// remove its origin while retaining the broker-created worktree root.
fn registered_orphan(host: &Host, container: Option<&Path>) -> PathBuf {
    let origin = tempfile::tempdir().unwrap();
    git(origin.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(origin.path().join("README.md"), "registered fixture\n").unwrap();
    std::fs::write(origin.path().join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
    git(origin.path(), &["add", "-A"]);
    git(origin.path(), &["commit", "-qm", "init"]);
    std::fs::create_dir(origin.path().join(".aethyme")).unwrap();
    std::fs::write(
        origin.path().join(".aethyme/broker.toml"),
        "[retention]\norphan_worktree_roots_days = 0\nartifact_reclaim_days = 0\n\
         artifact_sweep_budget_ms = 0\n",
    )
    .unwrap();

    let started = host.cli_from(
        origin.path(),
        container,
        &["start", "--task", "registered orphan", "--json"],
    );
    assert!(
        started.status.success(),
        "start: {}{}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    let root = worktree.parent().unwrap().to_path_buf();
    let expected_container = container
        .map(Path::to_path_buf)
        .unwrap_or_else(|| host.state.path().join("worktrees"));
    assert!(root.starts_with(expected_container.canonicalize().unwrap()));
    assert!(root.join(".aethyme-worktree-root.json").is_file());
    std::fs::create_dir(root.join("old-session")).unwrap();
    std::fs::write(root.join("old-session/leftover"), "registered bytes\n").unwrap();

    drop(origin);
    root
}

/// Every deleter in L0 slice C §2 that acts on host storage, through the
/// CLI: a session finished with its checkout kept, the startup sweep and
/// auto-removal, gc plan/apply with the orphan sweep, storage plan/apply and
/// attribution, and reclaim.
fn run_every_deleter(host: &Host, container: Option<&Path>, refused_gc_root: Option<&Path>) {
    let started = host.json(container, &["start", "--task", "cleanup reach", "--json"]);
    let id = started["id"].as_i64().unwrap().to_string();
    let finished = host.cli(container, &["finish", "--session", &id, "--keep-worktree"]);
    assert!(
        finished.status.success(),
        "finish: {}",
        String::from_utf8_lossy(&finished.stderr)
    );
    host.json(container, &["gc", "sweep", "--json"]);
    let plan = host.json(container, &["gc", "plan", "--full-scan", "--json"]);
    if let Some(refused_gc_root) = refused_gc_root {
        assert_eq!(plan["digest"].as_str(), Some(""), "{plan:#}");
        assert!(
            plan["deferred_checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|check| check == "orphan_scan"),
            "untrusted root provenance must defer orphan discovery: {plan:#}"
        );
        assert!(
            plan["blockers"].as_array().unwrap().iter().any(|blocker| {
                blocker["kind"] == "unmarked_worktree_root"
                    && blocker.to_string().contains("collaboration")
            }),
            "the overlapping collaboration root must be reported: {plan:#}"
        );
        let root_before_apply = snapshot(refused_gc_root);
        let unissued_confirmation = "a".repeat(64);
        let refused = host.cli(
            container,
            &[
                "gc",
                "apply",
                "--confirm",
                &unissued_confirmation,
                "--full-scan",
            ],
        );
        assert!(
            !refused.status.success(),
            "GC must refuse apply when orphan discovery is incomplete"
        );
        assert!(
            refused_gc_root.exists(),
            "the refused GC apply removed its root"
        );
        assert_eq!(
            snapshot(refused_gc_root),
            root_before_apply,
            "the refused GC apply changed the root subtree"
        );
    } else {
        let digest = plan["digest"].as_str().unwrap();
        assert_eq!(
            digest.len(),
            64,
            "complete plan must be applicable: {plan:#}"
        );
        let applied = host.cli(
            container,
            &["gc", "apply", "--confirm", digest, "--full-scan"],
        );
        assert!(
            applied.status.success(),
            "gc apply: {}",
            String::from_utf8_lossy(&applied.stderr)
        );
    }
    host.json(
        container,
        &["gc", "storage", "attribute", "--apply", "--json"],
    );
    let storage = host.json(container, &["gc", "storage", "plan", "--json"]);
    let storage_apply = host.json(
        container,
        &[
            "gc",
            "storage",
            "apply",
            "--confirm",
            storage["digest"].as_str().unwrap(),
            "--json",
        ],
    );
    if let Some(refused_gc_root) = refused_gc_root {
        let expected_path = refused_gc_root.to_string_lossy().into_owned();
        assert!(
            storage_apply["applied"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| {
                    item["kind"].as_str() == Some("orphan_root")
                        && item["path"].as_str() == Some(expected_path.as_str())
                }),
            "the later storage plan/apply lane should account for its orphan removal: {storage_apply:#}"
        );
    }
    let reclaim = host.json(container, &["gc", "reclaim", "plan", "--json"]);
    let reclaimed = host.cli(
        container,
        &[
            "gc",
            "reclaim",
            "apply",
            "--confirm",
            reclaim["digest"].as_str().unwrap(),
        ],
    );
    assert!(
        reclaimed.status.success(),
        "reclaim apply: {}",
        String::from_utf8_lossy(&reclaimed.stderr)
    );
}

#[test]
#[ignore = "child process of the cases below"]
fn child_opens_the_collaboration_store() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let repo = std::env::current_dir().unwrap();
    match open_for_repository(&repo, &ProjectKey::parse("proj-t").unwrap()) {
        Ok(store) => println!("RESULT ok {}", store.durability().receipt_label()),
        Err(error) => println!("RESULT err {}", error.code()),
    }
}

/// The default layout: the worktree container is `<state>/worktrees`, a
/// sibling of the collaboration root.
#[test]
fn retained_state_survives_every_legacy_deleter() {
    let host = Host::new();
    let project = host.seed();
    let before = snapshot(&host.collaboration());
    let orphan = registered_orphan(&host, None);

    run_every_deleter(&host, None, None);

    assert!(!orphan.exists(), "the orphan sweep did not run");
    assert_eq!(snapshot(&host.collaboration()), before);
    assert!(project.join("objects/sha256/ab/abcdef").is_file());
    assert!(host.open(None).is_ok());
}

/// A worktree container pointed at the host state directory itself makes the
/// collaboration root one of its children. Opening refuses, and the existing
/// state still survives: an unmarked child is reported, never removed or
/// adopted.
#[test]
fn a_container_over_the_host_state_is_refused_and_still_cannot_remove_it() {
    let host = Host::new();
    host.seed();
    let before = snapshot(&host.collaboration());
    let container = host.state.path().to_path_buf();
    let orphan = orphan_root(&container);

    assert_eq!(
        host.open(Some(&container)).unwrap_err(),
        "overlaps_cleanup_root"
    );
    let plan = host.json(Some(&container), &["gc", "plan", "--full-scan", "--json"]);
    assert!(
        plan["blockers"].as_array().unwrap().iter().any(|blocker| {
            blocker["kind"] == "unmarked_worktree_root"
                && blocker.to_string().contains("collaboration")
        }),
        "{plan:#}"
    );
    run_every_deleter(&host, Some(&container), Some(&orphan));

    assert!(
        !orphan.exists(),
        "the separately reviewed storage lane should reclaim its orphan"
    );
    assert_eq!(snapshot(&host.collaboration()), before);
    assert!(
        !host
            .collaboration()
            .join(".aethyme-worktree-root.json")
            .exists(),
        "attribution must not adopt the collaboration root"
    );
}

/// A container elsewhere is fine; the default container is still checked,
/// because a shell without the override sweeps it.
#[test]
fn an_external_container_leaves_the_default_layout_valid() {
    let host = Host::new();
    let external = tempfile::tempdir().unwrap();
    assert!(host.open(Some(external.path())).is_ok());
}

/// A scratch repository never writes into the implicit host state directory.
#[test]
fn an_ephemeral_repository_gets_no_implicit_state() {
    let host = Host::new();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "child_opens_the_collaboration_store",
        "--ignored",
        "--nocapture",
    ]);
    host.env(&mut command, None);
    let output = command
        .env_remove("AETHYME_HOST_STATE_DIR")
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("RESULT err ephemeral_repository"),
        "{stdout}"
    );
    assert_eq!(std::fs::read_dir(host.home.path()).unwrap().count(), 0);
}

/// A state directory named inside the platform default's worktree container
/// looks fine to this process, but a shell without the setting enumerates
/// that container and would list the collaboration root as an orphan
/// candidate. Opening refuses it.
#[test]
fn a_state_directory_inside_another_shells_container_is_refused() {
    let host = Host::new();
    let default_state = if cfg!(target_os = "macos") {
        host.home.path().join("Library/Application Support/Aethyme")
    } else {
        host.home.path().join(".local/state/aethyme")
    };
    let inner = default_state.join("worktrees/private-state");
    std::fs::create_dir_all(&inner).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "child_opens_the_collaboration_store",
        "--ignored",
        "--nocapture",
    ]);
    host.env(&mut command, None);
    let output = command
        .env("AETHYME_HOST_STATE_DIR", &inner)
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("RESULT err overlaps_cleanup_root"),
        "{stdout}"
    );
    assert!(!inner.join("collaboration").exists());
}
