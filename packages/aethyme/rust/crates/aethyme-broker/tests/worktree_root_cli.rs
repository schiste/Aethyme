use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::Broker;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

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

fn fixture_at(repo: &Path) {
    std::fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(repo, &["add", "README.md", ".gitignore"]);
    git(repo, &["commit", "-qm", "init"]);
}

fn run(repo: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .env("AETHYME_HOST_STATE_DIR", state)
        .env_remove("AETHYME_WORKTREE_ROOT")
        .output()
        .unwrap()
}

fn run_with_root(repo: &Path, root: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .env("AETHYME_WORKTREE_ROOT", root)
        .output()
        .unwrap()
}

fn json(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn start_places_worktree_outside_repository_under_a_private_owned_root() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let state = tmp.path().join("state");
    fixture_at(&repo);

    let started = json(run(
        &repo,
        &state,
        &["start", "--task", "scanner isolation", "--json"],
    ));
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    let root = PathBuf::from(started["worktree_placement"]["root"].as_str().unwrap());

    assert!(worktree.starts_with(state.canonicalize().unwrap()));
    assert!(!worktree.starts_with(repo.canonicalize().unwrap()));
    assert_eq!(worktree.parent(), Some(root.as_path()));
    assert_eq!(started["worktree_placement"]["source"], "host_state");
    assert_eq!(started["worktree_placement"]["outside_repository"], true);

    let marker = root.join(".aethyme-worktree-root.json");
    let marker_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    assert_eq!(marker_json["schema_version"], 1);
    assert_eq!(
        marker_json["repository_root"],
        repo.canonicalize().unwrap().to_string_lossy().as_ref()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn worktree_root_plan_is_read_only_and_distinguishes_same_named_clones() {
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("one/project");
    let second = tmp.path().join("two/project");
    let state = tmp.path().join("not-created");
    fixture_at(&first);
    fixture_at(&second);

    let first_plan = json(run(&first, &state, &["worktree-root", "--json"]));
    let second_plan = json(run(&second, &state, &["worktree-root", "--json"]));

    assert_ne!(first_plan["repository_key"], second_plan["repository_key"]);
    assert!(
        first_plan["repository_key"]
            .as_str()
            .unwrap()
            .starts_with("project-")
    );
    assert!(!state.exists(), "planning must not create host state");
}

#[test]
fn starts_from_a_broker_worktree_create_siblings_instead_of_nesting() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let state = tmp.path().join("state");
    fixture_at(&repo);

    let first = json(run(&repo, &state, &["start", "--task", "first", "--json"]));
    let first_path = PathBuf::from(first["worktree_path"].as_str().unwrap());
    let second = json(run(
        &first_path,
        &state,
        &["start", "--task", "second", "--json"],
    ));
    let second_path = PathBuf::from(second["worktree_path"].as_str().unwrap());

    assert_eq!(first_path.parent(), second_path.parent());
    assert!(!second_path.starts_with(&first_path));
}

#[test]
fn explicit_root_inside_a_linked_worktree_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let state = tmp.path().join("state");
    fixture_at(&repo);

    let first = json(run(&repo, &state, &["start", "--task", "owner", "--json"]));
    let first_path = PathBuf::from(first["worktree_path"].as_str().unwrap());
    let output = run_with_root(&repo, &first_path, &["start", "--task", "nested", "--json"]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("refusing nested broker worktree path"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unavailable_host_state_uses_reported_legacy_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let blocked_state = tmp.path().join("not-a-directory");
    fixture_at(&repo);
    std::fs::write(&blocked_state, "blocked\n").unwrap();

    let started = json(run(
        &repo,
        &blocked_state,
        &["start", "--task", "fallback", "--json"],
    ));
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());

    assert!(worktree.starts_with(repo.canonicalize().unwrap().join(".aethyme/worktrees")));
    assert_eq!(
        started["worktree_placement"]["source"],
        "repository_fallback"
    );
    assert_eq!(started["worktree_placement"]["outside_repository"], false);
    assert!(
        started["worktree_placement"]["fallback_reason"]
            .as_str()
            .unwrap()
            .contains("cannot prepare broker worktree root")
    );
}

#[test]
fn cleanup_accepts_a_worktree_owned_by_the_external_root_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let root = tmp.path().join("owned-worktrees");
    fixture_at(&repo);

    let mut broker = Broker::open(&repo).unwrap().with_worktree_root(&root);
    let session = broker.start_worktree("cleanup external checkout").unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    assert!(worktree.exists());

    broker.close(session.id).unwrap();
    broker.cleanup(session.id, false).unwrap();

    assert!(!worktree.exists());
    assert!(root.join(".aethyme-worktree-root.json").is_file());
}
