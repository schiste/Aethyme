//! Public CLI coverage for experimental project associations. Both
//! repositories begin as exact clones of one origin commit, then enroll and
//! bind independently before the local association is confirmed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_testkit::aethyme_bin;
use serde_json::Value;

struct Fixture {
    state: tempfile::TempDir,
    cache: tempfile::TempDir,
    home: tempfile::TempDir,
    worktrees: tempfile::TempDir,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "association test")
        .env("GIT_AUTHOR_EMAIL", "association@example.invalid")
        .env("GIT_COMMITTER_NAME", "association test")
        .env("GIT_COMMITTER_EMAIL", "association@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

impl Fixture {
    fn new() -> Self {
        Self {
            state: tempfile::tempdir().unwrap(),
            cache: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
            worktrees: tempfile::tempdir().unwrap(),
        }
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(aethyme_bin())
            .args(args)
            .current_dir(cwd)
            .env("HOME", self.home.path())
            .env("AETHYME_HOST_STATE_DIR", self.state.path())
            .env("AETHYME_HOST_CACHE_DIR", self.cache.path())
            .env("AETHYME_WORKTREE_ROOT", self.worktrees.path())
            .env(
                "AETHYME_CHAU7_MCP_BRIDGE",
                "/__aethyme_test_no_chau7_bridge__",
            )
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .output()
            .unwrap()
    }

    fn json(&self, cwd: &Path, args: &[&str]) -> (i32, Value) {
        let mut args = args.to_vec();
        args.push("--json");
        let output = self.run(cwd, &args);
        let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?}: invalid JSON ({error}):\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().unwrap(), value)
    }

    fn capture(&self, repo: &Path, name: &str) {
        let (code, started) = self.json(
            repo,
            &["broker", "start", "--task", name, "--short-name", name],
        );
        assert_eq!(code, 0, "{started:#}");
        let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(worktree.join("src/association.rs"), format!("// {name}\n")).unwrap();
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-qm", name]);
        let session = started["id"].as_i64().unwrap().to_string();
        let (_, submitted) = self.json(repo, &["broker", "submit", "--session", &session]);
        assert_eq!(
            submitted["collaboration_capture"]["status"], "acknowledged",
            "{submitted:#}"
        );
    }
}

fn clone_repo(origin: &Path, destination: &Path) {
    let output = Command::new("git")
        .args([
            "clone",
            "--quiet",
            origin.to_str().unwrap(),
            destination.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git clone: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn enroll_and_bind(fixture: &Fixture, repo: &Path, anchor: &str, suffix: &str) -> (String, String) {
    let (code, enrollment) = fixture.json(repo, &["collab", "enroll", "--write"]);
    assert_eq!(code, 0, "{enrollment:#}");
    let project_key = enrollment["project_key"].as_str().unwrap().to_string();
    let project_id = enrollment["project_id"].as_str().unwrap().to_string();
    git(repo, &["add", "-f", ".aethyme/config.toml"]);
    git(
        repo,
        &["commit", "-qm", &format!("enable collaboration {suffix}")],
    );

    let (code, proposed) = fixture.json(
        repo,
        &["collab", "project", "enroll", "--commit", anchor, "--write"],
    );
    assert_eq!(code, 0, "{proposed:#}");
    let descriptor_id = proposed["descriptor_record_id"]
        .as_str()
        .unwrap()
        .to_string();
    let config_path = repo.join(".aethyme/config.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config.push_str("descriptor = \".aethyme/project-descriptor.json\"\n");
    std::fs::write(&config_path, config).unwrap();
    git(
        repo,
        &[
            "add",
            "-f",
            ".aethyme/config.toml",
            ".aethyme/project-descriptor.json",
        ],
    );
    git(
        repo,
        &[
            "commit",
            "-qm",
            &format!("commit descriptor pointer {suffix}"),
        ],
    );

    fixture.capture(repo, &format!("association-{suffix}"));
    let (code, bound) = fixture.json(
        repo,
        &["collab", "project", "bind", "--confirm", &descriptor_id],
    );
    assert_eq!(code, 0, "{bound:#}");
    assert_eq!(bound["project_id"], project_id);
    assert_eq!(bound["descriptor_record_id"], descriptor_id);
    (project_key, project_id)
}

#[test]
fn independently_enrolled_playground_clones_confirm_persist_and_revoke_one_local_association() {
    let fixture = Fixture::new();
    let seed = tempfile::tempdir().unwrap();
    git(seed.path(), &["init", "-q", "-b", "main"]);
    let playground = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aethyme-broker/tests/fixtures/synthetic-playground-symbol-identity/seed");
    std::fs::create_dir_all(seed.path().join("src")).unwrap();
    for relative in ["src/lib.ts", "src/other.ts"] {
        std::fs::copy(playground.join(relative), seed.path().join(relative)).unwrap();
    }
    std::fs::write(seed.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::create_dir_all(seed.path().join(".aethyme")).unwrap();
    std::fs::write(seed.path().join(".aethyme/config.toml"), "").unwrap();
    git(seed.path(), &["add", "-A"]);
    git(seed.path(), &["add", "-f", ".aethyme/config.toml"]);
    git(seed.path(), &["commit", "-qm", "identical source seed"]);
    let anchor = git(seed.path(), &["rev-parse", "HEAD"]);
    let origin = tempfile::tempdir().unwrap();
    git(
        seed.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            ".",
            origin.path().to_str().unwrap(),
        ],
    );
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    clone_repo(origin.path(), left.path());
    clone_repo(origin.path(), right.path());
    assert_eq!(git(left.path(), &["rev-parse", "HEAD"]), anchor);
    assert_eq!(git(right.path(), &["rev-parse", "HEAD"]), anchor);
    assert_eq!(
        git(left.path(), &["rev-parse", "HEAD^{tree}"]),
        git(right.path(), &["rev-parse", "HEAD^{tree}"]),
        "the clones begin at the same source tree"
    );
    assert_eq!(
        git(left.path(), &["remote", "get-url", "origin"]),
        git(right.path(), &["remote", "get-url", "origin"]),
        "the clones share one origin URL"
    );

    let (left_key, left_project) = enroll_and_bind(&fixture, left.path(), &anchor, "left");
    let (right_key, right_project) = enroll_and_bind(&fixture, right.path(), &anchor, "right");
    assert_ne!(left_key, right_key, "independent local stores");
    assert_ne!(left_project, right_project, "independent ProjectIds");

    let (code, proposal) = fixture.json(
        left.path(),
        &[
            "collab",
            "project",
            "association",
            "propose",
            "--peer-repo",
            right.path().to_str().unwrap(),
            "--write",
        ],
    );
    assert_eq!(code, 0, "{proposal:#}");
    let association_id = proposal["association_record_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, confirmed) = fixture.json(
        left.path(),
        &[
            "collab",
            "project",
            "association",
            "confirm",
            "--peer-repo",
            right.path().to_str().unwrap(),
            "--confirm",
            &association_id,
        ],
    );
    assert_eq!(code, 0, "{confirmed:#}");
    assert_eq!(confirmed["peer_store_written"], false);
    assert_eq!(confirmed["grants_execution_authority"], false);
    assert_eq!(confirmed["grants_acceptance_authority"], false);

    // Each command opens a fresh process: the exact row survives reopen, and
    // its presence remains confined to the invoking project store.
    let (code, reopened) = fixture.json(
        left.path(),
        &[
            "collab",
            "project",
            "association",
            "confirm",
            "--peer-repo",
            right.path().to_str().unwrap(),
            "--confirm",
            &association_id,
        ],
    );
    assert_eq!(code, 0, "{reopened:#}");
    assert_eq!(reopened["status"], "already_confirmed");
    let (code, peer_missing) = fixture.json(
        right.path(),
        &[
            "collab",
            "project",
            "association",
            "revoke",
            "--confirm",
            &association_id,
        ],
    );
    assert_eq!(code, 3, "{peer_missing:#}");
    assert_eq!(peer_missing["code"], "association_missing");

    let (code, revoked) = fixture.json(
        left.path(),
        &[
            "collab",
            "project",
            "association",
            "revoke",
            "--confirm",
            &association_id,
        ],
    );
    assert_eq!(code, 0, "{revoked:#}");
    assert_eq!(revoked["status"], "revoked");
    let (code, reactivation) = fixture.json(
        left.path(),
        &[
            "collab",
            "project",
            "association",
            "confirm",
            "--peer-repo",
            right.path().to_str().unwrap(),
            "--confirm",
            &association_id,
        ],
    );
    assert_eq!(code, 3, "{reactivation:#}");
    assert_eq!(reactivation["code"], "association_revoked");
}
