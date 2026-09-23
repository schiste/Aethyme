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

fn reclaim_plan_json(repo: &Path, container: &Path) -> serde_json::Value {
    let output = run(repo, container, &["reclaim", "plan", "--json"]);
    assert!(
        output.status.success(),
        "reclaim plan: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn reclaim_plan_warns_but_prints_when_snapshot_storage_is_unavailable() {
    let (repo, container) = fixture("");
    let root_output = run(repo.path(), container.path(), &["worktree-root", "--json"]);
    assert!(root_output.status.success());
    let worktree_root =
        serde_json::from_slice::<serde_json::Value>(&root_output.stdout).unwrap()["preferred_root"]
            .as_str()
            .map(PathBuf::from)
            .unwrap();

    let first = reclaim_plan_json(repo.path(), container.path());
    let digest = first["digest"].as_str().unwrap();
    let snapshot = worktree_root.join(format!(".aethyme-reclaim-plan-{digest}.json"));
    std::fs::remove_file(&snapshot).unwrap();
    std::fs::create_dir(&snapshot).unwrap();

    let output = run(
        repo.path(),
        container.path(),
        &["reclaim", "plan", "--json"],
    );
    assert!(
        output.status.success(),
        "reclaim plan should remain available: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["digest"], digest);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("continuing with the digest-bound plan"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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
fn reclaim_confirmation_binds_decisions_not_sizes_and_explains_changes() {
    let (repo, container) = fixture("");
    let root_output = run(repo.path(), container.path(), &["worktree-root", "--json"]);
    assert!(
        root_output.status.success(),
        "worktree root: {}",
        String::from_utf8_lossy(&root_output.stderr)
    );
    let worktree_root =
        serde_json::from_slice::<serde_json::Value>(&root_output.stdout).unwrap()["preferred_root"]
            .as_str()
            .map(PathBuf::from)
            .unwrap();
    let target = worktree_root.join("session/target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("artifact"), "small\n").unwrap();

    let plan = reclaim_plan_json(repo.path(), container.path());
    let digest = plan["digest"].as_str().unwrap().to_string();

    // The reviewed deletion decision is unchanged even though the build output
    // grew after the plan was printed.
    std::fs::write(target.join("artifact"), "larger build output\n").unwrap();
    let apply = run(
        repo.path(),
        container.path(),
        &["reclaim", "apply", "--confirm", &digest],
    );
    assert!(
        apply.status.success(),
        "reclaim apply: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(!target.exists(), "the reviewed candidate should be removed");

    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("artifact"), "small\n").unwrap();
    let plan = reclaim_plan_json(repo.path(), container.path());
    let digest = plan["digest"].as_str().unwrap().to_string();
    let added = worktree_root.join("session/build");
    std::fs::create_dir_all(&added).unwrap();
    std::fs::write(added.join("artifact"), "new\n").unwrap();

    let refused = run(
        repo.path(),
        container.path(),
        &["reclaim", "apply", "--confirm", &digest],
    );
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(message.contains("added candidate"), "{message}");
    assert!(message.contains("build"), "{message}");
    assert!(target.exists(), "a changed plan must not remove candidates");
}

#[test]
fn reclaim_confirmation_keeps_digest_mismatch_primary_when_snapshot_is_unreadable() {
    let (repo, container) = fixture("");
    let root_output = run(repo.path(), container.path(), &["worktree-root", "--json"]);
    let worktree_root =
        serde_json::from_slice::<serde_json::Value>(&root_output.stdout).unwrap()["preferred_root"]
            .as_str()
            .map(PathBuf::from)
            .unwrap();

    let plan = reclaim_plan_json(repo.path(), container.path());
    let digest = plan["digest"].as_str().unwrap().to_string();
    let snapshot = worktree_root.join(format!(".aethyme-reclaim-plan-{digest}.json"));
    std::fs::write(&snapshot, b"not a reclaim snapshot\n").unwrap();

    let added = worktree_root.join("session/build");
    std::fs::create_dir_all(&added).unwrap();
    std::fs::write(added.join("artifact"), "new\n").unwrap();

    let refused = run(
        repo.path(),
        container.path(),
        &["reclaim", "apply", "--confirm", &digest],
    );
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        message.contains("confirmation does not match the current plan"),
        "{message}"
    );
    assert!(
        message.contains("saved review could not be read"),
        "{message}"
    );
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
        rendered.contains("no longer matches current state")
            || rendered.contains("orphan evidence changed"),
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
fn gc_plan_reports_large_ignored_directories_without_authorizing_them() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    std::fs::write(repo.path().join(".git/info/exclude"), "ignored-cache/\n").unwrap();
    let declined = worktree.join("ignored-cache");
    std::fs::create_dir_all(&declined).unwrap();
    std::fs::write(
        declined.join("dataset.bin"),
        vec![0_u8; aethyme_broker::UNCLASSIFIED_ARTIFACT_REPORT_THRESHOLD_BYTES as usize + 1],
    )
    .unwrap();

    let plan = plan_json(repo.path(), container.path());
    let reported = plan["declined_artifacts"].as_array().unwrap();
    assert_eq!(
        reported.len(),
        1,
        "expected one declined directory: {reported:?}"
    );
    assert_eq!(reported[0]["relative_dir"], "ignored-cache");
    let declined_bytes = reported[0]["estimated_bytes"].as_u64().unwrap();
    assert!(declined_bytes > aethyme_broker::UNCLASSIFIED_ARTIFACT_REPORT_THRESHOLD_BYTES);
    assert_eq!(plan["estimated_declined_artifact_bytes"], declined_bytes);
    assert!(
        plan["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|artifact| artifact["relative_dir"] != "ignored-cache")
    );

    let reclaimable_from_candidates = plan["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["estimated_bytes"].as_u64().unwrap())
        .chain(plan["files"].as_array().unwrap().iter().map(|file| {
            file["bytes_before"]
                .as_u64()
                .unwrap()
                .saturating_sub(file["bytes_after"].as_u64().unwrap())
        }))
        .chain(
            plan["worktrees"]
                .as_array()
                .unwrap()
                .iter()
                .map(|worktree| worktree["estimated_bytes"].as_u64().unwrap()),
        )
        .chain(
            plan["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|artifact| artifact["estimated_bytes"].as_u64().unwrap()),
        )
        .chain(
            plan["orphans"]
                .as_array()
                .unwrap()
                .iter()
                .map(|orphan| orphan["estimated_bytes"].as_u64().unwrap()),
        )
        .sum::<u64>();
    assert_eq!(
        plan["estimated_reclaimable_bytes"],
        reclaimable_from_candidates
    );
    assert!(declined.exists(), "reporting must not remove opaque output");
}

#[test]
fn configured_artifact_directories_are_reclaimable_in_gc_and_reclaim() {
    let (repo, container) = fixture(
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\nartefact_directories = [\".pnpm-store\"]\n",
    );
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    std::fs::write(repo.path().join(".git/info/exclude"), ".pnpm-store/\n").unwrap();
    let store = worktree.join(".pnpm-store");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("index"), b"package-index").unwrap();

    let plan = plan_json(repo.path(), container.path());
    assert!(
        plan["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|artifact| artifact["relative_dir"] == ".pnpm-store"),
        "configured cache should extend the built-in catalog: {}",
        serde_json::to_string_pretty(&plan).unwrap()
    );
    assert!(plan["declined_artifacts"].as_array().unwrap().is_empty());

    let reclaim_plan = reclaim_plan_json(repo.path(), container.path());
    assert!(
        reclaim_plan["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| candidate["path"]
                .as_str()
                .unwrap()
                .ends_with("/.pnpm-store")),
        "legacy reclaim should use the same configured catalog: {reclaim_plan}"
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
        !store.exists(),
        "configured cache should be removed after review"
    );
    assert!(worktree.join("work.txt").exists());
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
fn unknown_retention_fields_warn_without_disabling_gc_or_status() {
    let (repo, container) =
        fixture("[retention]\nartifact_sweep_budget_ms = 0\nfuture_sweep_days = 14\n");

    let plan = plan_json(repo.path(), container.path());
    assert_eq!(plan["policy"]["artifact_sweep_budget_ms"], 0);
    assert_eq!(
        plan["retention_config_warnings"][0]["field"],
        "retention.future_sweep_days"
    );

    let output = run(repo.path(), container.path(), &["status", "--json"]);
    assert!(
        output.status.success(),
        "status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        status["cleanup_retention"]["retention_config"]["warnings"][0]["field"],
        "retention.future_sweep_days"
    );
    assert!(status["advice"].as_array().unwrap().iter().any(|advice| {
        advice["id"] == "retention.config"
            && advice["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.as_str().unwrap().contains("future_sweep_days"))
    }));
}

#[test]
fn invalid_retention_config_is_explained_by_status_instead_of_hiding_the_error() {
    let (repo, container) = fixture("[retention]\nstartup_budget_ms = 0\n");

    let output = run(repo.path(), container.path(), &["status", "--json"]);
    assert!(
        output.status.success(),
        "status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        status["cleanup_retention"]["retention_config"]["error"]
            .as_str()
            .unwrap()
            .contains("startup_budget_ms")
    );
    assert!(status["advice"].as_array().unwrap().iter().any(|advice| {
        advice["id"] == "retention.config"
            && advice["summary"]
                .as_str()
                .unwrap()
                .contains("conservative defaults")
            && advice["commands"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.as_str().unwrap().contains("broker gc plan"))
    }));
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
    let retained_summary = plan["blocker_summary"]
        .as_array()
        .unwrap()
        .iter()
        .find(|summary| summary["kind"] == "retention_age")
        .expect("a recently closed unproven worktree should be age-blocked");
    assert_eq!(retained_summary["count"], 1);
    assert!(
        retained_summary["retained_bytes"].as_u64().unwrap() > 0,
        "age blocker should account for the retained worktree bytes: {retained_summary}"
    );
    let worktree_summary = plan["worktree_blocker_summary"]
        .as_array()
        .unwrap()
        .iter()
        .find(|summary| summary["kind"] == "retention_age")
        .expect("worktree-specific summary should include the age blocker");
    assert_eq!(worktree_summary["count"], 1);
    assert!(worktree_summary["retained_bytes"].as_u64().unwrap() > 0);

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

fn count_entries(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                1 + count_entries(&entry.path())
            } else {
                1
            }
        })
        .sum()
}

#[test]
fn a_budget_too_small_to_finish_still_makes_ground_and_keeps_the_cache_resumable() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let target = worktree.join("rust/target");
    for index in 0..3000_u32 {
        let bucket = target.join(format!("debug/deps/{}", index % 16));
        std::fs::create_dir_all(&bucket).unwrap();
        std::fs::write(bucket.join(format!("{index}.rlib")), b"artifact").unwrap();
    }
    let before = count_entries(&target);

    // A budget this small is spent before the removal even begins, which is
    // the shape of the original defect: a `target/` of this size takes minutes
    // to unlink and no budget on a broker-open path can hold one. The removal
    // must still leave the tree smaller than it found it, and still leave it
    // recognisable, or the sweep can never converge.
    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 1\n",
    )
    .unwrap();
    for _ in 0..5 {
        let output = run(repo.path(), container.path(), &["status"]);
        assert!(
            output.status.success(),
            "status: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            target.join("CACHEDIR.TAG").is_file(),
            "a partly removed cache must stay classifiable or no later pass resumes it"
        );
    }
    assert!(
        count_entries(&target) < before,
        "every pass must be worth at least one unlink"
    );

    // An unfinished pass withholds the cadence stamp, so the next open sweeps
    // again rather than waiting out the interval. Give one enough budget and
    // it finishes what the others started.
    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 30000\n",
    )
    .unwrap();
    let output = run(repo.path(), container.path(), &["status"]);
    assert!(
        output.status.success(),
        "status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !target.exists(),
        "the resumed sweep must finish the removal"
    );
    assert!(
        worktree.join("work.txt").exists(),
        "committed work must be untouched"
    );
}

/// An orphaned root's owning database is gone, which is what makes it an
/// orphan, so no age is recoverable for it and only its size can order it.
/// Path order -- what the sweep used before -- is arbitrary with respect to
/// what a time-bounded `gc apply` should reach first (#176).
#[test]
fn the_largest_orphaned_root_is_planned_before_smaller_ones() {
    let (repo, container) =
        fixture("[retention]\norphan_worktree_roots_days = 0\nartifact_sweep_budget_ms = 0\n");
    let missing = container.path().join("deleted-repository");
    // Named so that alphabetical order and size order disagree: if the plan
    // still sorted by path, `repo-a-small` would come first.
    let small = stamp_root(container.path(), "repo-a-small", &missing);
    let large = stamp_root(container.path(), "repo-z-large", &missing);
    std::fs::write(large.join("some-session/bulk.bin"), vec![b'x'; 512 * 1_024]).unwrap();

    let plan = plan_json(repo.path(), container.path());
    let keys: Vec<&str> = plan["orphans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|orphan| orphan["repository_key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["repo-z-large", "repo-a-small"]);
    assert!(small.exists() && large.exists(), "planning removes nothing");
}

/// An orphaned root's bytes are retained bytes, and `doctor` takes the
/// recorded-size path, which never walks one. If those missing bytes did not
/// count against the total's completeness, `doctor` would report a floor as a
/// total and the budget would read as satisfied because nobody looked (#176).
#[test]
fn doctor_counts_an_unsized_orphaned_root_against_its_own_totals() {
    let (repo, container) = fixture(
        "[retention]\norphan_worktree_roots_days = 0\nartifact_sweep_budget_ms = 0\nroutine_size_budget_ms = 0\nretained_bytes_budget = 1073741824\n",
    );
    let missing = container.path().join("gone-repository");
    stamp_root(container.path(), "repo-orphaned", &missing);

    // The expensive audit sizes it, so the orphan is a real, non-zero cost.
    let plan = plan_json(repo.path(), container.path());
    assert_eq!(plan["orphans"].as_array().unwrap().len(), 1);
    assert!(plan["orphans"][0]["estimated_bytes"].as_u64().unwrap() > 0);
    assert_eq!(plan["unmeasured_directory_count"].as_u64().unwrap(), 0);
    assert_eq!(plan["budget_verdict"].as_str().unwrap(), "within");

    let output = run(repo.path(), container.path(), &["doctor", "--json"]);
    assert!(
        output.status.success(),
        "doctor: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let retention = &report["retention"];
    assert_eq!(retention["unmeasured_directory_count"].as_u64().unwrap(), 1);
    assert_eq!(retention["budget_verdict"].as_str().unwrap(), "unknown");
    assert!(
        !retention["over_retained_bytes_budget"].as_bool().unwrap(),
        "undecided is not a breach"
    );
}
