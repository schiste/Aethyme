//! Host-level GC sweeps: orphaned worktree roots and reclaimable build caches.
//!
//! These run through the CLI shim so each case gets its own process and can set
//! `AETHYME_WORKTREE_ROOT` without mutating shared state in a threaded harness.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn fixture(retention: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let repo = tempfile::tempdir().unwrap();
    let container = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(
        repo.path().join(".gitignore"),
        "/.aethyme/\n/rust/target/\n",
    )
    .unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);
    std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
    std::fs::write(repo.path().join(".aethyme/broker.toml"), retention).unwrap();
    (repo, container)
}

fn run(repo: &Path, container: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .env("AETHYME_WORKTREE_ROOT", container)
        .output()
        .unwrap()
}

fn plan_json(repo: &Path, container: &Path) -> serde_json::Value {
    let output = run(repo, container, &["gc", "plan", "--json"]);
    assert!(
        output.status.success(),
        "gc plan: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A worktree root left behind by a repository that no longer exists.
fn stamp_root(container: &Path, key: &str, repository_root: &Path) -> PathBuf {
    let root = container.join(key);
    std::fs::create_dir_all(root.join("some-session")).unwrap();
    std::fs::write(root.join("some-session/leftover.txt"), "bytes\n").unwrap();
    std::fs::write(
        root.join(".aethyme-worktree-root.json"),
        serde_json::json!({
            "schema_version": 1,
            "repository_key": key,
            "repository_root": repository_root,
        })
        .to_string(),
    )
    .unwrap();
    root
}

#[test]
fn orphaned_roots_are_swept_while_owned_and_unmarked_roots_are_protected() {
    let (repo, container) =
        fixture("[retention]\norphan_worktree_roots_days = 0\nartifact_sweep_budget_ms = 0\n");
    let missing = container.path().join("deleted-repository");
    let orphan = stamp_root(container.path(), "repo-orphaned", &missing);
    let owned = stamp_root(container.path(), "repo-owned", repo.path());
    let unmarked = container.path().join("repo-unmarked");
    std::fs::create_dir_all(&unmarked).unwrap();
    std::fs::write(unmarked.join("stray.txt"), "bytes\n").unwrap();

    let plan = plan_json(repo.path(), container.path());
    let keys: Vec<&str> = plan["orphans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|orphan| orphan["repository_key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["repo-orphaned"]);
    assert!(plan["orphans"][0]["estimated_bytes"].as_u64().unwrap() > 0);
    assert!(
        plan["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["kind"] == "unmarked_worktree_root"),
        "a root with no breadcrumb must be reported, never removed blind"
    );

    let digest = plan["digest"].as_str().unwrap();
    let output = run(
        repo.path(),
        container.path(),
        &["gc", "apply", "--confirm", digest],
    );
    assert!(
        output.status.success(),
        "gc apply: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!orphan.exists(), "the orphaned root should be reclaimed");
    assert!(owned.exists(), "a root with a live owner must survive");
    assert!(unmarked.exists(), "an unmarked root must survive");
}

#[test]
fn a_reappearing_repository_revokes_an_authorized_orphan_removal() {
    let (repo, container) =
        fixture("[retention]\norphan_worktree_roots_days = 0\nartifact_sweep_budget_ms = 0\n");
    let missing = container.path().join("deleted-repository");
    let orphan = stamp_root(container.path(), "repo-orphaned", &missing);

    let plan = plan_json(repo.path(), container.path());
    let digest = plan["digest"].as_str().unwrap().to_owned();
    assert_eq!(plan["orphans"].as_array().unwrap().len(), 1);

    // The premise of the authorization was that nothing owns this tree.
    std::fs::create_dir_all(&missing).unwrap();

    let output = run(
        repo.path(),
        container.path(),
        &["gc", "apply", "--confirm", &digest],
    );
    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Either guard is a correct refusal: the plan is re-derived when no journal
    // is outstanding, so the digest stops it before the per-item evidence check
    // that protects a resumed apply. What must hold either way is that an
    // authorization stops binding the moment its premise changes.
    assert!(
        rendered.contains("confirmation mismatch") || rendered.contains("orphan evidence changed"),
        "expected the removal to be refused, got {rendered}"
    );
    assert!(orphan.exists(), "the reclaimed tree must survive");
}

/// Start a session, leave an unaccepted commit on it, and close it keeping the
/// worktree. The result is a retained worktree whose provenance is blocked.
fn blocked_session_with_build_cache(repo: &Path, container: &Path) -> (String, PathBuf) {
    let output = run(repo, container, &["start", "--task", "blocked", "--json"]);
    assert!(
        output.status.success(),
        "start: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = session["id"].as_i64().unwrap().to_string();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());

    // An unaccepted commit is what makes cleanup refuse this worktree.
    std::fs::write(worktree.join("work.txt"), "work\n").unwrap();
    git(&worktree, &["add", "work.txt"]);
    git(&worktree, &["commit", "-qm", "work"]);

    let target = worktree.join("rust/target");
    std::fs::create_dir_all(target.join("debug")).unwrap();
    std::fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
    std::fs::write(target.join("debug/artifact.bin"), vec![0_u8; 4096]).unwrap();

    // `close` rather than `finish`: an unaccepted commit is exactly what makes
    // `finish` refuse, and a blocked-but-closed session is the case under test.
    let output = run(repo, container, &["close", "--session", &id]);
    assert!(
        output.status.success(),
        "close: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (id, worktree)
}

/// Turn the autonomous sweep on after setup.
///
/// Fixtures start with it disabled so the opens that build the scenario cannot
/// consume the sweep's cadence window before the assertion runs.
fn enable_sweep(repo: &Path) {
    std::fs::write(
        repo.join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\n",
    )
    .unwrap();
}

#[test]
fn default_policy_reclaims_closed_session_build_caches() {
    let (repo, container) = fixture("");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let target = worktree.join("rust/target");

    let output = run(repo.path(), container.path(), &["status", "--json"]);
    assert!(
        output.status.success(),
        "status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !target.exists(),
        "closed-session build cache should be gone"
    );
    assert!(
        worktree.join("work.txt").exists(),
        "default reclamation must preserve committed work"
    );
}

#[test]
fn explicit_opt_out_preserves_closed_session_build_caches() {
    let (repo, container) = fixture("[retention]\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let target = worktree.join("rust/target");

    let output = run(repo.path(), container.path(), &["status", "--json"]);
    assert!(
        output.status.success(),
        "status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(target.exists(), "explicit opt-out must preserve the cache");
}

#[test]
fn tracked_directory_with_a_cache_witness_is_never_reclaimed() {
    let (repo, container) = fixture("");
    let output = run(
        repo.path(),
        container.path(),
        &["start", "--task", "tracked target", "--json"],
    );
    assert!(output.status.success());
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = session["id"].as_i64().unwrap().to_string();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    let target = worktree.join("rust/target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
    std::fs::write(target.join("tracked.txt"), "repository content\n").unwrap();
    git(
        &worktree,
        &[
            "add",
            "-f",
            "rust/target/CACHEDIR.TAG",
            "rust/target/tracked.txt",
        ],
    );
    git(&worktree, &["commit", "-qm", "tracked target"]);
    let output = run(repo.path(), container.path(), &["close", "--session", &id]);
    assert!(output.status.success());

    let output = run(repo.path(), container.path(), &["status", "--json"]);
    assert!(output.status.success());
    assert!(
        target.join("tracked.txt").exists(),
        "a tracked directory must survive even when its name and witness resemble a cache"
    );
}

#[test]
fn build_caches_are_reclaimable_even_when_the_worktree_itself_is_blocked() {
    // The autonomous sweep is off so the plan is what is under test.
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());

    let plan = plan_json(repo.path(), container.path());
    let artifacts = plan["artifacts"].as_array().unwrap();
    assert_eq!(
        artifacts.len(),
        1,
        "expected one build cache, got {artifacts:?}"
    );
    assert_eq!(artifacts[0]["relative_dir"], "rust/target");
    assert!(artifacts[0]["estimated_bytes"].as_u64().unwrap() >= 4096);

    // The worktree is blocked from whole-worktree cleanup, yet its build cache
    // is still reclaimable: the block protects commits, which a cache has none of.
    assert!(
        plan["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .all(|worktree| worktree["session_id"].as_i64().unwrap().to_string() != id),
        "a session with unaccepted commits must not be scheduled for removal"
    );

    let digest = plan["digest"].as_str().unwrap();
    let output = run(
        repo.path(),
        container.path(),
        &["gc", "apply", "--confirm", digest],
    );
    assert!(
        output.status.success(),
        "gc apply: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !worktree.join("rust/target").exists(),
        "build cache should be gone"
    );
    assert!(
        worktree.join("work.txt").exists(),
        "committed work must be untouched"
    );
}

#[test]
fn the_autonomous_sweep_reclaims_build_caches_without_confirmation() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let target = worktree.join("rust/target");
    assert!(target.exists());
    enable_sweep(repo.path());

    // Any broker command opens the broker, which runs the sweep.
    let output = run(repo.path(), container.path(), &["status"]);
    assert!(
        output.status.success(),
        "status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !target.exists(),
        "an idle closed session's build cache should be reclaimed unprompted"
    );
    assert!(
        worktree.join("work.txt").exists(),
        "committed work must be untouched"
    );
}

#[test]
fn a_live_session_keeps_its_build_cache() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let output = run(
        repo.path(),
        container.path(),
        &["start", "--task", "live", "--json"],
    );
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    let target = worktree.join("rust/target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();

    // Without this the sweep would simply be disabled, and the assertion below
    // would hold for the wrong reason.
    enable_sweep(repo.path());

    let output = run(repo.path(), container.path(), &["status"]);
    assert!(output.status.success());
    assert!(
        target.exists(),
        "a session still in use must keep its build cache"
    );
}
