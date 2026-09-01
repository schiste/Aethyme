use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_testkit::{aethyme_bin, tmp_dir};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Aethyme Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Aethyme Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    run_with_env(repo, args, None)
}

fn run_with_env(repo: &Path, args: &[&str], extra: Option<(&str, &str)>) -> Output {
    let binary = aethyme_bin();
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![binary.parent().unwrap().to_path_buf()];
    paths.extend(std::env::split_paths(&existing_path));
    let mut command = Command::new(&binary);
    command
        .args(args)
        .current_dir(repo)
        .env("XDG_CONFIG_HOME", repo.join("config-home"))
        .env("AETHYME_HOST_STATE_DIR", repo.join(".git/host-state"))
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("GIT_AUTHOR_NAME", "Aethyme Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Aethyme Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid");
    if let Some((key, value)) = extra {
        command.env(key, value);
    }
    command.output().unwrap()
}

struct Fixture {
    _root: tempfile::TempDir,
    seed: PathBuf,
    clone: PathBuf,
}

fn fixture() -> Fixture {
    let root = tmp_dir();
    let remote = root.path().join("remote.git");
    let seed = root.path().join("seed");
    let clone = root.path().join("clone");
    assert!(
        Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&remote)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&seed)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(seed.join("README.md"), "fixture\n").unwrap();
    git(&seed, &["add", "README.md"]);
    git(&seed, &["commit", "-qm", "initial"]);
    git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&seed, &["push", "-qu", "origin", "main"]);
    git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    assert!(
        Command::new("git")
            .args(["clone", "-q"])
            .arg(&remote)
            .arg(&clone)
            .status()
            .unwrap()
            .success()
    );
    Fixture {
        _root: root,
        seed,
        clone,
    }
}

fn plan(repo: &Path) -> serde_json::Value {
    let output = run(repo, &["deploy", "plan", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn plan_is_read_only_digest_bound_and_based_on_current_upstream() {
    let fixture = fixture();
    let before_refs = git(&fixture.clone, &["show-ref"]);
    let before_status = git(&fixture.clone, &["status", "--short"]);

    std::fs::write(fixture.seed.join("upstream.txt"), "advanced\n").unwrap();
    git(&fixture.seed, &["add", "upstream.txt"]);
    git(&fixture.seed, &["commit", "-qm", "advance upstream"]);
    git(&fixture.seed, &["push", "-q", "origin", "main"]);
    let upstream = git(&fixture.seed, &["rev-parse", "HEAD"]);

    let report = plan(&fixture.clone);
    let repeated = plan(&fixture.clone);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["remote_base"]["exact_sha"], upstream);
    assert_eq!(report["local_behind_upstream_commits"], 1);
    assert_eq!(report["generated_tree"]["from_schema"], 0);
    assert_eq!(report["safe"], true);
    assert_eq!(report["hook_manager"]["kind"], "absent");
    assert_eq!(report["plan_digest"].as_str().unwrap().len(), 64);
    assert_eq!(report["plan_digest"], repeated["plan_digest"]);
    assert!(
        report["planned_paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "AGENTS.md")
    );
    assert_eq!(git(&fixture.clone, &["show-ref"]), before_refs);
    assert_eq!(git(&fixture.clone, &["status", "--short"]), before_status);
    assert!(!fixture.clone.join(".aethyme").exists());
}

#[test]
fn plan_distinguishes_disjoint_overlap_and_foreign_hook_ownership() {
    let fixture = fixture();
    std::fs::write(fixture.clone.join("notes.local"), "keep me\n").unwrap();
    let disjoint = plan(&fixture.clone);
    assert_eq!(disjoint["safe"], true);
    assert_eq!(disjoint["disjoint_dirty_paths"][0], "notes.local");

    std::fs::write(fixture.clone.join("AGENTS.md"), "custom\n").unwrap();
    let overlap = plan(&fixture.clone);
    assert_eq!(overlap["safe"], false);
    assert_eq!(overlap["overlapping_dirty_paths"][0], "AGENTS.md");

    std::fs::remove_file(fixture.clone.join("AGENTS.md")).unwrap();
    git(&fixture.clone, &["config", "core.hooksPath", ".husky"]);
    let hooks = plan(&fixture.clone);
    assert_eq!(hooks["safe"], false);
    assert_eq!(hooks["hook_manager"]["kind"], "foreign");
    let encoded = serde_json::to_string(&hooks).unwrap();
    assert!(!encoded.contains(".husky"));
    assert!(encoded.contains("will not overwrite"));
}

#[test]
fn confirmed_execute_preserves_publishes_verifies_syncs_and_resumes() {
    let fixture = fixture();
    let original = git(&fixture.clone, &["rev-parse", "HEAD"]);
    let planned = plan(&fixture.clone);
    let digest = planned["plan_digest"].as_str().unwrap();

    let executed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&executed.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["completed"], true);
    assert_eq!(
        report["local_main_synchronized"],
        true,
        "report: {report}; status: {}",
        git(&fixture.clone, &["status", "--short"])
    );
    assert_eq!(
        report["publication_sha"], report["verified_remote_sha"],
        "publication is verified from remote evidence"
    );

    let remote_main = git(&fixture.seed, &["ls-remote", "origin", "refs/heads/main"])
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert_eq!(git(&fixture.clone, &["rev-parse", "main"]), remote_main);
    assert_eq!(
        git(
            &fixture.clone,
            &["show", &format!("{remote_main}:.aethyme/repository.json")]
        )
        .contains("schema_version"),
        true
    );
    let preserved = report["preservation_refs"][0]["ref_name"].as_str().unwrap();
    assert_eq!(git(&fixture.clone, &["rev-parse", preserved]), original);

    let resumed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(resumed.status.success());
    let resumed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["publication_sha"], report["publication_sha"]);
    assert_eq!(resumed["queue_entry_id"], report["queue_entry_id"]);
}

#[test]
fn execute_publishes_but_does_not_sync_a_dirty_primary_checkout() {
    let fixture = fixture();
    let original = git(&fixture.clone, &["rev-parse", "HEAD"]);
    std::fs::write(fixture.clone.join("notes.local"), "uncommitted\n").unwrap();
    let planned = plan(&fixture.clone);
    assert_eq!(planned["safe"], true);
    let digest = planned["plan_digest"].as_str().unwrap();

    let executed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&executed.stdout).unwrap();
    assert_eq!(report["local_main_synchronized"], false);
    assert_eq!(git(&fixture.clone, &["rev-parse", "main"]), original);
    assert_eq!(
        std::fs::read_to_string(fixture.clone.join("notes.local")).unwrap(),
        "uncommitted\n"
    );
    assert_ne!(report["verified_remote_sha"], original);
}

#[test]
fn stale_main_enrolls_from_upstream_and_preexisting_sibling_detects_activation() {
    let fixture = fixture();
    let original = git(&fixture.clone, &["rev-parse", "HEAD"]);
    let sibling = fixture._root.path().join("pre-enrollment-sibling");
    git(
        &fixture.clone,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            sibling.to_str().unwrap(),
            &original,
        ],
    );
    std::fs::write(fixture.seed.join("upstream.txt"), "preserve upstream\n").unwrap();
    git(&fixture.seed, &["add", "upstream.txt"]);
    git(&fixture.seed, &["commit", "-qm", "advance upstream"]);
    git(&fixture.seed, &["push", "-q", "origin", "main"]);

    let planned = plan(&fixture.clone);
    assert_eq!(planned["local_behind_upstream_commits"], 1);
    let digest = planned["plan_digest"].as_str().unwrap();
    let executed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&executed.stdout).unwrap();
    assert_eq!(report["local_main_synchronized"], true);
    let local_main = git(&fixture.clone, &["rev-parse", "main"]);
    assert_eq!(
        git(
            &fixture.clone,
            &["show", &format!("{local_main}:upstream.txt")]
        ),
        "preserve upstream"
    );
    let pre_push = fixture.clone.join(".git/hooks/pre-push");
    assert!(
        pre_push.is_file(),
        "{} was not installed",
        pre_push.display()
    );
    let hook = std::fs::read_to_string(&pre_push).unwrap();
    assert!(hook.contains("broker hooks pre-push"), "{hook}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            std::fs::metadata(&pre_push).unwrap().permissions().mode() & 0o111,
            0,
            "{} is not executable",
            pre_push.display()
        );
    }
    assert_eq!(
        PathBuf::from(git(
            &sibling,
            &["rev-parse", "--git-path", "hooks/pre-push"]
        ))
        .canonicalize()
        .unwrap(),
        pre_push.canonicalize().unwrap()
    );
    assert_eq!(
        Command::new("git")
            .args(["config", "--get", "core.hooksPath"])
            .current_dir(&sibling)
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    git(&sibling, &["fetch", "-q", "origin", "main"]);
    git(&sibling, &["checkout", "-q", "--detach", "origin/main"]);

    let commit = Command::new("git")
        .args(["commit", "--allow-empty", "-m", "unmanaged sibling commit"])
        .current_dir(&sibling)
        .env("GIT_AUTHOR_NAME", "Aethyme Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Aethyme Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let push = Command::new("git")
        .args(["push", "origin", "HEAD:main"])
        .current_dir(&sibling)
        .output()
        .unwrap();
    assert!(!push.status.success());
    let refusal = String::from_utf8_lossy(&push.stderr);
    assert!(
        refusal.contains("Aethyme") && refusal.contains("protected"),
        "{refusal}"
    );
}

#[test]
fn remote_movement_after_review_refuses_before_preservation() {
    let fixture = fixture();
    let planned = plan(&fixture.clone);
    let digest = planned["plan_digest"].as_str().unwrap();
    std::fs::write(fixture.seed.join("moved.txt"), "moved\n").unwrap();
    git(&fixture.seed, &["add", "moved.txt"]);
    git(&fixture.seed, &["commit", "-qm", "move remote"]);
    git(&fixture.seed, &["push", "-q", "origin", "main"]);

    let refused = run(&fixture.clone, &["deploy", "execute", "--confirm", digest]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("changed after review"));
    assert!(
        git(
            &fixture.clone,
            &["for-each-ref", "refs/heads/aethyme/preservation"]
        )
        .is_empty()
    );
}

#[test]
fn persisted_phase_boundaries_resume_without_duplicate_publication() {
    for phase in ["preserved", "outputs_applied", "promoted", "published"] {
        let fixture = fixture();
        let planned = plan(&fixture.clone);
        let digest = planned["plan_digest"].as_str().unwrap();
        let interrupted = run_with_env(
            &fixture.clone,
            &["deploy", "execute", "--confirm", digest],
            Some(("AETHYME_TEST_ENROLLMENT_STOP_AFTER_PHASE", phase)),
        );
        assert!(!interrupted.status.success(), "phase {phase}");
        assert!(
            String::from_utf8_lossy(&interrupted.stderr).contains("test interruption"),
            "phase {phase}: {}",
            String::from_utf8_lossy(&interrupted.stderr)
        );

        let resumed = run(
            &fixture.clone,
            &["deploy", "execute", "--confirm", digest, "--json"],
        );
        assert!(
            resumed.status.success(),
            "phase {phase}: {}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
        assert_eq!(report["completed"], true, "phase {phase}");
        assert_eq!(
            report["publication_sha"], report["verified_remote_sha"],
            "phase {phase}"
        );
    }
}
