//! Implementation-blind tests for skill deployment and the generated
//! onboarding artifacts.
//!
//! Ported from `tests/indexing/test_skills.py` (python-retirement
//! Phase 7). That file had already been converted from
//! `src.indexing.skills` / `src.indexing.onboarding` imports to router
//! invocations when those modules moved to the aethyme-enhance crate
//! (Phase 3); this port keeps it driving the binary.

use std::path::Path;

use aethyme_testkit::repos::{read, write};
use aethyme_testkit::{invoke_aethyme, tmp_dir};
use serde_json::Value;

fn compile_skills(repo: &Path) -> String {
    invoke_aethyme(["repo", "compile-skills", &repo.display().to_string()])
        .ok()
        .to_string()
}

fn artifact(repo: &Path, relative: &str) -> Value {
    serde_json::from_str(&read(repo.join(relative))).expect("generated artifact is JSON")
}

fn target_repo(tmp: &Path) -> std::path::PathBuf {
    let repo = tmp.join("target-repo");
    std::fs::create_dir_all(&repo).unwrap();
    repo
}

#[test]
fn deploy_skills_installs_only_runtime_navigation_skill() {
    let tmp = tmp_dir();
    let repo = target_repo(tmp.path());

    let result = invoke_aethyme(["repo", "deploy-skills", &repo.display().to_string()]);
    result.ok();
    result.assert_contains("Deployed skills: aethyme");
    assert!(repo.join(".codex/skills/aethyme/SKILL.md").is_file());
    assert!(!repo.join(".codex/skills/eval").exists());
}

#[test]
fn deploy_skills_remove_sweeps_deployed_skills() {
    let tmp = tmp_dir();
    let repo = target_repo(tmp.path());
    let repo_arg = repo.display().to_string();
    invoke_aethyme(["repo", "deploy-skills", &repo_arg]);

    let result = invoke_aethyme(["repo", "deploy-skills", &repo_arg, "--remove"]);
    result.ok();
    result.assert_contains("Removed skills: aethyme");
    assert!(!repo.join(".codex/skills/aethyme").exists());
}

#[test]
fn compile_skills_renders_repo_specific_artifacts() {
    let tmp = tmp_dir();
    let repo = target_repo(tmp.path());
    write(
        repo.join("package.json"),
        r#"{"name": "demo", "scripts": {"test": "vitest", "dev": "vite"}}"#,
    );
    write(repo.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n");
    write(repo.join("src/main.ts"), "console.log('demo')\n");

    let output = compile_skills(&repo);

    for relative in [
        ".aethyme/generated/onboarding.json",
        ".aethyme/generated/act-starter.json",
        ".codex/skills/repo-onboarding/SKILL.md",
        ".claude/skills/repo-onboarding/SKILL.md",
        ".codex/skills/repo-act/SKILL.md",
        ".claude/skills/repo-act/SKILL.md",
    ] {
        assert!(
            output.contains(&format!("  compiled   {relative}")),
            "compile-skills did not report {relative}:\n{output}"
        );
        assert!(repo.join(relative).is_file(), "missing {relative}");
    }

    let onboarding = artifact(&repo, ".aethyme/generated/onboarding.json");
    let act = artifact(&repo, ".aethyme/generated/act-starter.json");
    assert_eq!(onboarding["repo"]["package_manager"], "pnpm");
    assert!(
        onboarding["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command["command"] == "pnpm test")
    );
    assert_eq!(onboarding["primary_commands"]["fast_test"], "pnpm test");
    assert_eq!(onboarding["primary_commands"]["dev"], "pnpm dev");
    assert_eq!(
        onboarding["primary_entrypoints"]["app"]["path"],
        "src/main.ts"
    );
    assert!(
        onboarding["areas"]
            .as_array()
            .unwrap()
            .iter()
            .any(|area| area["path"] == "src")
    );
    assert!(
        !onboarding["summon"]["recommended_when"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        onboarding["telemetry"]["counts"]["commands"]
            .as_i64()
            .unwrap()
            >= 1
    );
    assert_eq!(act["recommended_mode"], "repo-act");
    assert_eq!(act["commands"]["fast_test"], "pnpm test");
    assert_eq!(act["commands"]["dev"], "pnpm dev");
    assert_eq!(act["primary_entrypoints"]["app"]["path"], "src/main.ts");
    assert!(read(repo.join(".codex/skills/repo-onboarding/SKILL.md")).contains("Repo Onboarding"));
    assert!(read(repo.join(".codex/skills/repo-act/SKILL.md")).contains("## Debugging Checklist"));
}

#[test]
fn onboarding_overrides_replace_selected_sections() {
    let tmp = tmp_dir();
    let repo = target_repo(tmp.path());
    write(repo.join("pyproject.toml"), "[project]\nname = 'demo'\n");
    write(
        repo.join(".aethyme/overrides/onboarding.json"),
        &serde_json::json!({
            "repo": {"kind": "service"},
            "commands": [{
                "kind": "test",
                "command": "./scripts/test-fast.sh",
                "source": "manual-override",
                "confidence": "high",
            }],
            "summon": {
                "recommended_when": ["first touch", "broad task"],
                "skip_when": ["single known file"],
            },
            "notes": [
                "Use sandbox credentials from 1Password.",
                "Do not edit generated files under src/gen directly.",
            ],
        })
        .to_string(),
    );

    compile_skills(&repo);
    let onboarding = artifact(&repo, ".aethyme/generated/onboarding.json");

    assert_eq!(onboarding["repo"]["kind"], "service");
    assert_eq!(
        onboarding["commands"],
        serde_json::json!([{
            "kind": "test",
            "command": "./scripts/test-fast.sh",
            "source": "manual-override",
            "confidence": "high",
        }])
    );
    assert_eq!(
        onboarding["summon"]["recommended_when"],
        serde_json::json!(["first touch", "broad task"])
    );
    assert_eq!(
        onboarding["notes"],
        serde_json::json!([
            "Use sandbox credentials from 1Password.",
            "Do not edit generated files under src/gen directly.",
        ])
    );
    assert_eq!(onboarding["telemetry"]["overrides_applied"], true);
    assert_eq!(
        onboarding["telemetry"]["override_source"],
        ".aethyme/overrides/onboarding.json"
    );
    assert_eq!(onboarding["telemetry"]["counts"]["notes"], 2);

    let skill = read(repo.join(".codex/skills/repo-onboarding/SKILL.md"));
    assert!(skill.contains("## Maintainer Notes"));
    assert!(skill.contains("Use sandbox credentials from 1Password."));
}

#[test]
fn onboarding_collects_ranked_primary_commands_from_multiple_sources() {
    let tmp = tmp_dir();
    let repo = target_repo(tmp.path());
    write(
        repo.join("package.json"),
        &serde_json::json!({
            "name": "demo",
            "scripts": {
                "dev": "vite",
                "test": "vitest",
                "test:e2e": "playwright test",
                "lint": "eslint .",
                "build": "vite build",
            },
        })
        .to_string(),
    );
    write(repo.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n");
    write(
        repo.join("Justfile"),
        "install:\n\tpnpm install\n\ntest-fast:\n\tpnpm vitest run\n\nfull-test:\n\tpnpm playwright test\n",
    );
    write(
        repo.join("Procfile"),
        "web: pnpm dev --host 0.0.0.0\nworker: node workers/job.js\n",
    );
    write(repo.join("workers/job.js"), "console.log('worker')\n");
    write(
        repo.join("docker-compose.yml"),
        "services:\n  app:\n    image: node:20\n  test:\n    image: node:20\n",
    );

    compile_skills(&repo);
    let onboarding = artifact(&repo, ".aethyme/generated/onboarding.json");
    let act = artifact(&repo, ".aethyme/generated/act-starter.json");

    assert_eq!(onboarding["primary_commands"]["install"], "just install");
    assert_eq!(onboarding["primary_commands"]["dev"], "pnpm dev");
    assert_eq!(
        onboarding["primary_commands"]["fast_test"],
        "just test-fast"
    );
    assert_eq!(
        onboarding["primary_commands"]["full_test"],
        "just full-test"
    );
    assert_eq!(onboarding["primary_commands"]["lint"], "pnpm lint");
    assert_eq!(onboarding["primary_commands"]["build"], "pnpm build");
    assert_eq!(
        onboarding["primary_entrypoints"]["app"]["path"],
        "Procfile:web"
    );
    assert_eq!(
        onboarding["primary_entrypoints"]["worker"]["path"],
        "Procfile:worker"
    );
    let test_entrypoint = onboarding["primary_entrypoints"]["test"]["path"]
        .as_str()
        .unwrap();
    assert!(
        ["docker-compose.yml:test", "package.json:scripts.test"].contains(&test_entrypoint),
        "unexpected test entrypoint {test_entrypoint:?}"
    );
    let commands = onboarding["commands"].as_array().unwrap();
    assert!(
        commands
            .iter()
            .any(|command| command["source"] == "Procfile:web")
    );
    assert!(
        commands
            .iter()
            .any(|command| command["source"] == "docker-compose.yml")
    );
    assert_eq!(act["commands"]["fast_test"], "just test-fast");
    assert_eq!(act["commands"]["full_test"], "just full-test");
    assert_eq!(act["commands"]["dev"], "pnpm dev");
    assert_eq!(act["primary_entrypoints"]["app"]["path"], "Procfile:web");
}

#[test]
fn onboarding_collects_ranked_primary_entrypoints_from_multiple_sources() {
    let tmp = tmp_dir();
    let repo = target_repo(tmp.path());
    write(
        repo.join("package.json"),
        &serde_json::json!({
            "name": "demo",
            "main": "./src/server.ts",
            "bin": {"demo": "./bin/demo.js"},
            "scripts": {
                "dev": "node src/server.ts",
                "worker": "python workers/job.py",
                "test": "vitest",
            },
        })
        .to_string(),
    );
    write(repo.join("src/server.ts"), "console.log('server')\n");
    write(repo.join("bin/demo.js"), "console.log('cli')\n");
    write(repo.join("workers/job.py"), "print('job')\n");
    write(
        repo.join("Procfile"),
        "web: node src/server.ts\nworker: python workers/job.py\ntest: pnpm vitest\n",
    );

    compile_skills(&repo);
    let onboarding = artifact(&repo, ".aethyme/generated/onboarding.json");
    let act = artifact(&repo, ".aethyme/generated/act-starter.json");

    assert_eq!(
        onboarding["primary_entrypoints"]["app"]["path"],
        "src/server.ts"
    );
    assert_eq!(
        onboarding["primary_entrypoints"]["cli"]["path"],
        "bin/demo.js"
    );
    assert_eq!(
        onboarding["primary_entrypoints"]["worker"]["path"],
        "Procfile:worker"
    );
    let test_entrypoint = onboarding["primary_entrypoints"]["test"]["path"]
        .as_str()
        .unwrap();
    assert!(
        ["Procfile:test", "package.json:scripts.test"].contains(&test_entrypoint),
        "unexpected test entrypoint {test_entrypoint:?}"
    );
    assert!(
        onboarding["entrypoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entrypoint| entrypoint["path"] == "workers/job.py")
    );
    assert_eq!(act["primary_entrypoints"]["cli"]["path"], "bin/demo.js");
}
