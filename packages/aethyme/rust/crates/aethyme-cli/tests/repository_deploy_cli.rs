use std::fs;
use std::process::Command;

use aethyme_testkit::{aethyme_bin, tmp_dir};

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
        "AGENTS.md",
        "CLAUDE.md",
        ".codex/skills/aethyme/SKILL.md",
        ".claude/skills/aethyme/SKILL.md",
    ] {
        assert!(repo.join(relative).is_file(), "missing {relative}");
    }
    let agents = fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert!(agents.contains("## Broker Coordination"));
    assert!(!agents.contains("AETHYME_ROOT"));

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
    assert!(String::from_utf8_lossy(&verified.stderr).contains("Verification failed"));
    assert_eq!(fs::read_dir(&repo).unwrap().count(), before);
}

#[test]
fn top_level_help_exposes_the_canonical_deployment_surface() {
    let help = Command::new(aethyme_bin()).arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stderr).contains("deploy [verify]"));

    let deploy_help = Command::new(aethyme_bin())
        .args(["deploy", "--help"])
        .output()
        .unwrap();
    assert!(deploy_help.status.success());
    assert!(String::from_utf8_lossy(&deploy_help.stdout).contains("aethyme deploy verify"));
}
