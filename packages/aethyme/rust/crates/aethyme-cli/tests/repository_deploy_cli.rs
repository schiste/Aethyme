use std::fs;
use std::process::Command;

use aethyme_testkit::{aethyme_bin, repo_root, tmp_dir};

fn repository(root: &std::path::Path) -> std::path::PathBuf {
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    let initialized = Command::new("git")
        .args(["init", "-b", "main"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(initialized.status.success());
    let committed = Command::new("git")
        .args([
            "-c",
            "user.name=Aethyme Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(committed.status.success());
    repo
}

fn command(repo: &std::path::Path) -> Command {
    let mut command = Command::new(aethyme_bin());
    command
        .env_remove("AETHYME_ROOT")
        .env("XDG_CONFIG_HOME", repo.join("empty-config"));
    command
}

fn commit_all(repo: &std::path::Path, message: &str) {
    let added = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(added.status.success());
    let committed = Command::new("git")
        .args([
            "-c",
            "user.name=Aethyme Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            message,
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );
}

fn status(repo: &std::path::Path) -> String {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn deploy_enrolls_and_verifies_a_repository_without_a_source_checkout() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    let deployed = command(&repo)
        .args(["deploy", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        deployed.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&deployed.stdout),
        String::from_utf8_lossy(&deployed.stderr)
    );

    for relative in [
        ".aethyme/config.toml",
        ".aethyme/repository.json",
        "AGENTS.md",
        "CLAUDE.md",
        ".codex/skills/aethyme/SKILL.md",
        ".claude/skills/aethyme/SKILL.md",
    ] {
        assert!(repo.join(relative).is_file(), "missing {relative}");
    }
    let config = fs::read_to_string(repo.join(".aethyme/config.toml")).unwrap();
    assert!(!config.contains("[graph]"));
    assert!(!repo.join(".aethyme/engine-version").exists());
    assert!(!repo.join(".aethyme/graph").exists());
    assert!(!repo.join(".aethyme/graph_store.redb").exists());
    let agents = fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert!(agents.contains("## Broker Coordination"));
    assert!(!agents.contains("AETHYME_ROOT"));
    let onboarding = fs::read_to_string(repo.join(".aethyme/generated/onboarding.json")).unwrap();
    assert!(onboarding.contains("\"root\": \".\""));
    assert!(!onboarding.contains(repo.to_string_lossy().as_ref()));
    let gitignore = fs::read_to_string(repo.join(".gitignore")).unwrap();
    for runtime_path in [
        ".aethyme/graph_store.redb",
        ".aethyme/graph_store.redb.indexing",
        ".aethyme/generated/experience-telemetry.jsonl",
        ".aethyme/generated/experience-status.json",
        ".aethyme/generated/experience-status.md",
    ] {
        assert!(gitignore.contains(runtime_path), "missing {runtime_path}");
        let ignored = Command::new("git")
            .args(["check-ignore", "--quiet", runtime_path])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(
            ignored.success(),
            "runtime path is not ignored: {runtime_path}"
        );
    }
    let committed_fragment = Command::new("git")
        .args(["check-ignore", "--quiet", ".aethyme/graph/example.rs.bin"])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(
        !committed_fragment.success(),
        "committed graph fragments must remain visible to Git"
    );
    let canonical_is_ignored = Command::new("git")
        .args([
            "check-ignore",
            "--quiet",
            ".aethyme/generated/onboarding.json",
        ])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(!canonical_is_ignored.success());
    let stdout = String::from_utf8_lossy(&deployed.stdout);
    assert!(stdout.contains("Review and commit repository policy:"));
    assert!(stdout.contains("Ignored machine-local runtime state:"));
    assert!(stdout.contains(".aethyme/generated/onboarding.json"));
    assert!(stdout.contains("Optional local Claude integration:"));
    assert!(stdout.contains("settings.local.json (machine-local; never commit)"));
    assert!(stdout.contains("graph authority disabled (optional)"));

    let verified = command(&repo)
        .args(["deploy", "verify", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
}

#[test]
fn canonical_clean_clone_verifies_without_ignored_claude_state() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    let deployed = command(&repo)
        .args(["deploy", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        deployed.status.success(),
        "{}",
        String::from_utf8_lossy(&deployed.stderr)
    );

    fs::remove_dir_all(repo.join(".claude")).unwrap();
    commit_all(&repo, "chore(aethyme): enroll portable policy");
    let clone = temp.path().join("clean-clone");
    let cloned = Command::new("git")
        .args(["clone", "--quiet"])
        .arg(&repo)
        .arg(&clone)
        .output()
        .unwrap();
    assert!(cloned.status.success());
    assert!(!clone.join(".claude/settings.local.json").exists());

    let verified = command(&clone)
        .args(["deploy", "verify", "--repo"])
        .arg(&clone)
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&verified.stdout),
        String::from_utf8_lossy(&verified.stderr)
    );
    let stdout = String::from_utf8_lossy(&verified.stdout);
    assert!(stdout.contains("[WARN] .claude/settings.local.json"));
    assert!(stdout.contains("optional local integration not installed"));
}

#[test]
fn deploy_in_a_linked_worktree_never_mutates_the_primary_checkout() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    let linked = temp.path().join("linked");
    let added = Command::new("git")
        .args(["worktree", "add", "-q", "-b", "agent/deploy"])
        .arg(&linked)
        .arg("main")
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let deployed = command(&linked)
        .args(["deploy", "--repo"])
        .arg(&linked)
        .output()
        .unwrap();
    assert!(
        deployed.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&deployed.stdout),
        String::from_utf8_lossy(&deployed.stderr)
    );

    for relative in [
        ".gitignore",
        ".aethyme/config.toml",
        ".aethyme/repository.json",
        "AGENTS.md",
        "CLAUDE.md",
    ] {
        assert!(linked.join(relative).is_file(), "missing {relative}");
        assert!(
            !repo.join(relative).exists(),
            "primary checkout was mutated at {relative}"
        );
    }
    assert!(
        repo.join(".aethyme/broker.db").is_file(),
        "broker runtime state should remain shared"
    );
    assert!(!linked.join(".aethyme/broker.db").exists());
}

#[test]
fn deploy_verify_is_read_only_and_rejects_missing_policy() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    let before = fs::read_dir(&repo).unwrap().count();

    let verified = command(&repo)
        .args(["deploy", "verify", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();

    assert!(!verified.status.success());
    let stderr = String::from_utf8_lossy(&verified.stderr);
    assert!(stderr.contains("missing .aethyme/repository.json"));
    assert!(stderr.contains("aethyme upgrade plan"));
    assert_eq!(fs::read_dir(&repo).unwrap().count(), before);
}

#[test]
fn top_level_help_exposes_the_canonical_deployment_surface() {
    let help = Command::new(aethyme_bin()).arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stderr).contains("deploy [verify|bridge]"));
    assert!(String::from_utf8_lossy(&help.stderr).contains("upgrade plan|apply|recover"));

    let deploy_help = Command::new(aethyme_bin())
        .args(["deploy", "--help"])
        .output()
        .unwrap();
    assert!(deploy_help.status.success());
    let deploy_help = String::from_utf8_lossy(&deploy_help.stdout);
    assert!(deploy_help.contains("aethyme deploy plan"));
    assert!(deploy_help.contains("aethyme deploy execute"));
    assert!(deploy_help.contains("--confirm <plan-sha256>"));
    assert!(deploy_help.contains("aethyme deploy verify"));
    assert!(deploy_help.contains("--local-only"));
    assert!(deploy_help.contains("--with-graph"));
    assert!(deploy_help.contains("--graph-repository <owner/name>"));
}

#[test]
fn graph_generation_is_opt_in_deferred_and_canonical_only() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    let deployed = command(&repo)
        .args([
            "deploy",
            "--repo",
            repo.to_str().unwrap(),
            "--with-graph",
            "--graph-repository",
            "example/project",
        ])
        .output()
        .unwrap();
    assert!(
        deployed.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&deployed.stdout),
        String::from_utf8_lossy(&deployed.stderr)
    );
    let config = fs::read_to_string(repo.join(".aethyme/config.toml")).unwrap();
    assert!(config.contains("[graph]"));
    assert!(config.contains("authority = \"committed_fragments\""));
    assert!(config.contains("repository = \"example/project\""));
    assert_eq!(
        fs::read_to_string(repo.join(".aethyme/engine-version")).unwrap(),
        format!("{}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(!repo.join(".aethyme/graph").exists());
    assert!(!repo.join(".aethyme/graph_store.redb").exists());
    let stdout = String::from_utf8_lossy(&deployed.stdout);
    assert!(stdout.contains("deferred  graph generation"));
    assert!(stdout.contains("commit the deployment"));
    assert!(stdout.contains("aethyme graph refresh plan --repo . --diff"));
    assert!(stdout.contains("graph authority is enabled but fragments are not committed"));
    assert!(!stdout.contains("aethyme-graph-index"));

    let local_temp = tmp_dir();
    let local_repo = repository(local_temp.path());
    let rejected = command(&local_repo)
        .args([
            "deploy",
            "--local-only",
            "--with-graph",
            "--graph-repository",
            "example/project",
            "--repo",
            local_repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("local-only activation remains graph-free")
    );
    assert!(!local_repo.join(".aethyme").exists());
}

#[test]
fn local_only_activation_is_clean_and_does_not_follow_a_clone() {
    let temp = tmp_dir();
    let repo = repository(temp.path());

    let bridge = command(&repo)
        .args(["deploy", "bridge", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        bridge.status.success(),
        "{}",
        String::from_utf8_lossy(&bridge.stderr)
    );
    for document in ["AGENTS.md", "CLAUDE.md"] {
        let text = fs::read_to_string(repo.join(document)).unwrap();
        assert!(text.contains(".aethyme/local/enabled"));
        assert!(text.contains("do not run Aethyme"));
    }
    commit_all(&repo, "docs: add inert local activation bridge");

    let activated = command(&repo)
        .args(["deploy", "--local-only", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        activated.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&activated.stdout),
        String::from_utf8_lossy(&activated.stderr)
    );
    assert!(repo.join(".aethyme/local/enabled").is_file());
    let policy = fs::read_to_string(repo.join(".aethyme/local/AGENTS.md")).unwrap();
    assert!(policy.contains("## Broker Coordination"));
    assert!(repo.join(".codex/skills/aethyme/SKILL.md").is_file());
    assert!(repo.join(".claude/skills/aethyme/SKILL.md").is_file());
    assert!(
        fs::read_to_string(repo.join(".claude/settings.local.json"))
            .unwrap()
            .contains(".claude/hooks/aethyme-load-context.sh")
    );
    assert!(!repo.join(".gitignore").exists());
    assert_eq!(status(&repo), "");

    let redeployed = command(&repo)
        .args(["deploy", "--local-only", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        redeployed.status.success(),
        "{}",
        String::from_utf8_lossy(&redeployed.stderr)
    );
    assert_eq!(status(&repo), "");

    let before = [
        ".aethyme/local/enabled",
        ".aethyme/local/AGENTS.md",
        ".aethyme/generated/onboarding.json",
    ]
    .map(|relative| fs::read(repo.join(relative)).unwrap());
    let verified = command(&repo)
        .args(["deploy", "verify", "--local-only", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let after = [
        ".aethyme/local/enabled",
        ".aethyme/local/AGENTS.md",
        ".aethyme/generated/onboarding.json",
    ]
    .map(|relative| fs::read(repo.join(relative)).unwrap());
    assert_eq!(after, before, "local verification wrote activation state");

    let clone = temp.path().join("clone");
    let cloned = Command::new("git")
        .args(["clone", "--quiet"])
        .arg(&repo)
        .arg(&clone)
        .output()
        .unwrap();
    assert!(cloned.status.success());
    assert!(clone.join("AGENTS.md").is_file());
    assert!(!clone.join(".aethyme/local/enabled").exists());
    assert!(!clone.join(".codex/skills/aethyme/SKILL.md").exists());
    let clone_verify = command(&clone)
        .args(["deploy", "verify", "--local-only", "--repo"])
        .arg(&clone)
        .output()
        .unwrap();
    assert!(!clone_verify.status.success());
}

#[test]
fn local_only_requires_the_committed_bridge_before_writing() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    let deployed = command(&repo)
        .args(["deploy", "--local-only", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(!deployed.status.success());
    assert!(
        String::from_utf8_lossy(&deployed.stderr).contains("requires the committed inert bridge")
    );
    assert!(!repo.join(".aethyme").exists());
    assert_eq!(status(&repo), "");
}

#[test]
fn local_only_refuses_to_mask_tracked_agent_policy() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    let bridge = command(&repo)
        .args(["deploy", "bridge", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(bridge.status.success());
    fs::create_dir_all(repo.join(".codex/skills/aethyme")).unwrap();
    fs::write(
        repo.join(".codex/skills/aethyme/SKILL.md"),
        "tracked maintainer policy\n",
    )
    .unwrap();
    commit_all(&repo, "docs: add bridge and maintainer policy");

    let deployed = command(&repo)
        .args(["deploy", "--local-only", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(!deployed.status.success());
    assert!(
        String::from_utf8_lossy(&deployed.stderr).contains("refuses to overwrite tracked policy")
    );
    assert!(!repo.join(".aethyme").exists());
    assert_eq!(status(&repo), "");
}

#[test]
fn oss_ci_enforces_self_contained_repository_deployment() {
    let workflow = fs::read_to_string(repo_root().join(".github/workflows/oss-ci.yml")).unwrap();
    assert!(workflow.contains("aethyme deploy --repo ."));
    assert!(workflow.contains("aethyme deploy verify --repo ."));
    assert!(workflow.contains("deployed policy embeds the build checkout"));
    assert!(workflow.contains("deployed policy embeds the target checkout"));
    assert!(!workflow.contains("export AETHYME_ROOT="));
}
