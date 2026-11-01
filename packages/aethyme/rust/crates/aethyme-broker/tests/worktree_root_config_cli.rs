//! `[worktrees] root` in `.aethyme/config.toml`: a repository-configured place
//! for session worktrees -- an external drive, say -- that supersedes the
//! per-user default only while it is usable, and what the broker does when it
//! is missing, full, or unplugged after use.
//!
//! Every run gets its own host state directory (the default location) and no
//! `AETHYME_WORKTREE_ROOT`, so the repository key is the input under test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::GitRepo;

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).into_owned()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    host: PathBuf,
    drive: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::write(
        repo.join(".aethyme/broker.toml"),
        "[retention]\nartifact_sweep_budget_ms = 0\nstartup_budget_ms = 5\nclosed_worktree_grace_hours = 0\n",
    )
    .unwrap();
    let host = tmp.path().join("host");
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&host).unwrap();
    Fixture {
        repo: repo.canonicalize().unwrap(),
        host,
        drive,
        _tmp: tmp,
    }
}

impl Fixture {
    fn configure(&self, section: &str) {
        std::fs::write(
            self.repo.join(".aethyme/config.toml"),
            format!("schema = 1\n\n[worktrees]\n{section}"),
        )
        .unwrap();
    }

    fn configure_root(&self, root: &Path) {
        self.configure(&format!("root = {:?}\n", root.to_str().unwrap()));
    }

    fn run(&self, args: &[&str]) -> Output {
        common::broker_cli(CLI, args)
            .current_dir(&self.repo)
            .env("AETHYME_HOST_STATE_DIR", &self.host)
            .env("AETHYME_HOST_CACHE_DIR", self._tmp.path().join("cache"))
            .env_remove("AETHYME_WORKTREE_ROOT")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn start(&self, task: &str) -> serde_json::Value {
        serde_json::from_str(&self.ok(&["start", "--task", task, "--json"])).unwrap()
    }

    fn assert_default_with_reason(&self, started: &serde_json::Value, needle: &str) {
        let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
        assert!(
            canonical(&worktree).starts_with(canonical(&self.host.join("worktrees"))),
            "{started}"
        );
        assert_eq!(started["worktree_placement"]["source"], "host_state");
        let reason = started["worktree_placement"]["fallback_reason"]
            .as_str()
            .unwrap_or_else(|| panic!("no fallback reason: {started}"));
        assert!(reason.contains(needle), "{reason}");
    }
}

fn add_sparse_fixture_files(fx: &Fixture) {
    for (relative, contents) in [
        ("src/included/file.rs", "included source"),
        ("src/planned/current.rs", "planned source"),
        ("src/omitted/deep/file.rs", "omitted proof"),
    ] {
        let path = fx.repo.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    git(&fx.repo, &["add", "-A"]);
    git(&fx.repo, &["commit", "-qm", "add sparse checkout fixtures"]);
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[test]
fn without_the_key_worktrees_stay_in_the_default_location() {
    let fx = fixture();
    let started = fx.start("default");
    assert_eq!(started["worktree_placement"]["source"], "host_state");
    assert!(started["worktree_placement"]["fallback_reason"].is_null());
}

#[test]
fn sparse_checkout_is_opt_in_and_leased_paths_are_present_without_false_deletions() {
    let fx = fixture();
    add_sparse_fixture_files(&fx);
    fx.configure("sparse_paths = [\"src/included\"]\n");

    let output = fx.run(&[
        "start",
        "--task",
        "sparse checkout with a leased path",
        "--path",
        "src/planned/new.rs",
        "--json",
    ]);
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let started: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    let base = started["start_base"]["commit"].as_str().unwrap();
    assert!(
        worktree.join("README.md").is_file(),
        "cone mode includes root files"
    );
    assert!(worktree.join("src/included/file.rs").is_file());
    assert!(worktree.join("src/planned/current.rs").is_file());
    assert!(
        !worktree.join("src/omitted/deep/file.rs").exists(),
        "unselected tracked files should stay absent"
    );

    let checkout = GitRepo::discover(&worktree).unwrap();
    assert!(checkout.dirty_paths().unwrap().is_empty());
    assert_eq!(
        checkout
            .file_at_commit(base, "src/omitted/deep/file.rs")
            .unwrap()
            .as_deref(),
        Some("omitted proof"),
        "source proofs must still read omitted files from the selected commit"
    );
    std::fs::write(worktree.join("src/included/file.rs"), "edited source").unwrap();
    assert_eq!(
        checkout.dirty_paths().unwrap(),
        vec!["src/included/file.rs"],
        "sparse omissions must not appear as deletions"
    );
    git(&worktree, &["checkout", "--", "src/included/file.rs"]);
    assert!(checkout.dirty_paths().unwrap().is_empty());
    GitRepo::discover(&fx.repo)
        .unwrap()
        .worktree_remove(&worktree, false)
        .unwrap();
    assert!(
        !worktree.exists(),
        "clean sparse worktrees remain removable"
    );
}

#[test]
fn missing_sparse_input_falls_back_to_a_full_checkout_with_a_warning() {
    let fx = fixture();
    add_sparse_fixture_files(&fx);
    fx.configure("sparse_paths = [\"src/missing\"]\n");

    let output = fx.run(&["start", "--task", "missing sparse input", "--json"]);
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let warning = String::from_utf8_lossy(&output.stderr);
    assert!(warning.contains("src/missing"), "{warning}");
    assert!(warning.contains("full checkout"), "{warning}");
    let started: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    assert!(worktree.join("src/included/file.rs").is_file());
    assert!(worktree.join("src/planned/current.rs").is_file());
    assert!(worktree.join("src/omitted/deep/file.rs").is_file());
    assert!(
        GitRepo::discover(&worktree)
            .unwrap()
            .dirty_paths()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn full_checkout_remains_the_default_without_sparse_paths() {
    let fx = fixture();
    add_sparse_fixture_files(&fx);

    let started = fx.start("full checkout default");
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    assert!(worktree.join("src/included/file.rs").is_file());
    assert!(worktree.join("src/planned/current.rs").is_file());
    assert!(worktree.join("src/omitted/deep/file.rs").is_file());
}

#[test]
fn an_available_configured_root_supersedes_the_default_and_is_locked() {
    let fx = fixture();
    std::fs::create_dir_all(&fx.drive).unwrap();
    fx.configure_root(&fx.drive);

    let started = fx.start("on the drive");
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    assert!(
        canonical(&worktree).starts_with(canonical(&fx.drive)),
        "{started}"
    );
    assert_eq!(started["worktree_placement"]["source"], "repository_config");
    assert!(started["worktree_placement"]["fallback_reason"].is_null());
    // A lock keeps `git worktree prune` from dropping the registration while
    // the drive is unplugged.
    let listing = git(&fx.repo, &["worktree", "list", "--porcelain"]);
    assert!(listing.contains("locked"), "{listing}");
}

#[test]
fn a_missing_root_falls_back_with_a_reason_and_is_never_created() {
    let fx = fixture();
    fx.configure_root(&fx.drive);

    let started = fx.start("drive is unplugged");
    fx.assert_default_with_reason(&started, "does not exist");
    assert!(
        !fx.drive.exists(),
        "an unmounted drive must not be replaced by an empty directory"
    );
}

#[test]
fn a_root_below_its_free_space_floor_falls_back() {
    let fx = fixture();
    std::fs::create_dir_all(&fx.drive).unwrap();
    fx.configure(&format!(
        "root = {:?}\nmin_free_bytes = {}\n",
        fx.drive.to_str().unwrap(),
        u64::MAX / 2
    ));

    let started = fx.start("drive is full");
    fx.assert_default_with_reason(&started, "min_free_bytes");
}

/// A fallback must preserve the recovery reserve too: simulating low space
/// for every placement refuses the start instead of bypassing admission.
#[test]
fn all_roots_reported_nearly_full_refuse_instead_of_bypassing_the_default_floor() {
    let fx = fixture();
    std::fs::create_dir_all(&fx.drive).unwrap();
    fx.configure_root(&fx.drive);

    let output = common::broker_cli(CLI, &["start", "--task", "drive is nearly full", "--json"])
        .current_dir(&fx.repo)
        .env("AETHYME_HOST_STATE_DIR", &fx.host)
        .env("AETHYME_HOST_CACHE_DIR", fx._tmp.path().join("cache"))
        .env_remove("AETHYME_WORKTREE_ROOT")
        .env(aethyme_broker::TEST_AVAILABLE_BYTES_ENV, "1024")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    let error = json_error::message(&output);
    assert!(error.contains("broker recovery reserve"), "{error}");
    assert!(
        !fx.repo.join(".aethyme/worktrees").exists(),
        "fallback checkout allocated despite low space"
    );
}

#[test]
fn a_root_inside_the_repository_falls_back() {
    let fx = fixture();
    let inside = fx.repo.join("worktrees-here");
    std::fs::create_dir_all(&inside).unwrap();
    fx.configure_root(&inside);

    let started = fx.start("inside the repo");
    fx.assert_default_with_reason(&started, "inside repository");
}

#[test]
fn an_invalid_section_is_ignored_with_a_reason() {
    let fx = fixture();
    fx.configure("root = \"relative/worktrees\"\n");

    let started = fx.start("relative root");
    fx.assert_default_with_reason(&started, "absolute");
}

#[test]
fn the_environment_override_still_wins_over_the_repository_key() {
    let fx = fixture();
    std::fs::create_dir_all(&fx.drive).unwrap();
    fx.configure_root(&fx.drive);
    let explicit = fx.host.parent().unwrap().join("explicit");
    std::fs::create_dir_all(&explicit).unwrap();

    let output = common::broker_cli(CLI, &["start", "--task", "explicit", "--json"])
        .current_dir(&fx.repo)
        .env("AETHYME_HOST_STATE_DIR", &fx.host)
        .env("AETHYME_WORKTREE_ROOT", &explicit)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let started: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        started["worktree_placement"]["source"],
        "environment_override"
    );
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    assert!(canonical(&worktree).starts_with(canonical(&explicit)));
}

/// Unplugging the drive must not read as the worktree having been deleted:
/// cleanup keeps the session's branch and records, and everything is intact
/// when the drive comes back.
#[test]
fn a_worktree_on_an_unplugged_root_is_blocked_from_cleanup_not_removed() {
    let fx = fixture();
    std::fs::create_dir_all(&fx.drive).unwrap();
    fx.configure_root(&fx.drive);
    let started = fx.start("work on the drive");
    let id = started["id"].as_i64().unwrap().to_string();
    let branch = started["branch"].as_str().unwrap().to_string();
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    std::fs::write(worktree.join("work.txt"), "unpushed\n").unwrap();
    git(&worktree, &["add", "work.txt"]);
    git(&worktree, &["commit", "-qm", "work"]);
    fx.ok(&["finish", "close", "--session", &id]);

    let unplugged = fx.drive.with_extension("unplugged");
    std::fs::rename(&fx.drive, &unplugged).unwrap();

    let plan: serde_json::Value =
        serde_json::from_str(&fx.ok(&["finish", "cleanup", "--all-cleaned", "--json"])).unwrap();
    let item = plan["plan"]["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["session_id"].as_i64().unwrap().to_string() == id)
        .unwrap_or_else(|| panic!("session {id} missing from the plan: {plan}"));
    assert_eq!(item["disposition"], "inspection_failed", "{item}");
    assert!(
        item["reason"].as_str().unwrap().contains("unavailable"),
        "{item}"
    );

    let refused = fx.run(&["finish", "cleanup", &id]);
    assert!(
        !refused.status.success(),
        "cleanup of an unreachable worktree must refuse"
    );
    assert!(
        !git(&fx.repo, &["branch", "--list", &branch])
            .trim()
            .is_empty(),
        "the session branch survives"
    );

    std::fs::rename(&unplugged, &fx.drive).unwrap();
    assert_eq!(
        std::fs::read_to_string(worktree.join("work.txt")).unwrap(),
        "unpushed\n"
    );
    let listing = git(&fx.repo, &["worktree", "list", "--porcelain"]);
    assert!(
        listing.contains(worktree.file_name().unwrap().to_str().unwrap()),
        "the registration survived the unplugged period: {listing}"
    );
}

/// The broker releases its own lock when it removes a worktree from the
/// configured root, so cleanup still completes once the drive is present.
#[test]
fn cleanup_releases_the_brokers_own_lock_on_the_configured_root() {
    let fx = fixture();
    std::fs::create_dir_all(&fx.drive).unwrap();
    fx.configure_root(&fx.drive);
    let started = fx.start("finish on the drive");
    let id = started["id"].as_i64().unwrap().to_string();
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    fx.ok(&["finish", "close", "--session", &id]);

    fx.ok(&["finish", "cleanup", &id, "--force"]);
    assert!(!worktree.exists(), "the locked worktree was removed");
}

#[path = "common/json_error.rs"]
mod json_error;
