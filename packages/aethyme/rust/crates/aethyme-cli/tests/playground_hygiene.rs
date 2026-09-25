//! Contract checks for playground setup/verification hygiene.
//!
//! Ported from `tests/local/test_playground_hygiene_scripts.py`
//! (python-retirement Phase 7). Half the file deploys into a fixture
//! repo and inspects the generated root guidance; half greps the two
//! playground shell scripts, whose assertions mirror the deployed
//! templates (Class-3 protocol: template wording and the scripts'
//! patterns must move together, or the health check silently stops
//! checking).

use std::path::{Path, PathBuf};

use aethyme_testkit::repos::{build_pnpm_demo_repo, read, write};
use aethyme_testkit::{invoke_aethyme, package_root, tmp_dir};

/// Frozen contract path (was `src.enhance.AGENTS_OVERRIDE_PATH`; the
/// Python module is deleted — the aethyme-enhance crate owns the
/// constant now).
const AGENTS_OVERRIDE_PATH: &str = ".aethyme/overrides/agents.json";

fn scripts_dir() -> PathBuf {
    package_root().join("scripts/eval")
}

fn legacy_generated_agents_text() -> String {
    r#"# Agent Instructions

This repository is **Aethyme-enhanced**. For navigation, caller tracing,
dead-code analysis, or task localization, prefer Aethyme's high-level
Explore surface before brute-force grep.

## Quick start (any agent)

```bash
AETHYME_ROOT="/Users/example/Downloads/Repositories/Aethyme/packages/aethyme"
"$AETHYME_ROOT/.venv/bin/python" -m src.cli explore \
    --repo "$PWD" --request "<your task>" --format answer-json
```

## Detailed reference

Same content lives at both of these per-product skill paths.

## Verifying this enhancement

```bash
"$AETHYME_ROOT/.venv/bin/python" -m src.cli enhance verify --repo "$PWD"
```
"#
    .to_string()
}

fn assert_native_root_guidance(repo: &Path) {
    for filename in ["AGENTS.md", "CLAUDE.md"] {
        let text = read(repo.join(filename));
        assert!(text.contains("aethyme explore"));
        assert!(!text.contains("AETHYME_ROOT"));
        assert!(!text.contains("/rust/target/release/aethyme"));
        // Recovery plan P4.5 (2026-09-25): the root file prescribes the one
        // Explore command; the temp-file projection flow (and the
        // "Do not run `python -m src.cli`" notice) moved to the skill.
        assert!(text.contains("--format brief"));
        assert!(text.contains("navigation aid, not an answer"));
        assert!(!text.contains(r#""$AETHYME_ROOT/.venv/bin/python" -m src.cli explore"#));
        assert!(
            !text.contains(".venv/bin/python"),
            "{filename} reaches for the venv"
        );
        assert!(
            !text.contains("-m src.cli"),
            "{filename} names the Python CLI"
        );
    }
}

fn demo_repo(tmp: &Path) -> PathBuf {
    build_pnpm_demo_repo(&tmp.join("demo-repo"))
}

#[test]
fn enhance_deploy_root_guidance_uses_native_explore() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());

    invoke_aethyme(["enhance", "deploy", "--repo", &repo.display().to_string()]).ok();

    assert_native_root_guidance(&repo);
}

#[test]
fn enhance_deploy_does_not_migrate_legacy_generated_agents_as_maintainer() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    write(repo.join("AGENTS.md"), &legacy_generated_agents_text());

    invoke_aethyme(["enhance", "deploy", "--repo", &repo.display().to_string()]).ok();

    assert!(!read(repo.join("AGENTS.md")).contains("## Maintainer Notes"));
    assert_native_root_guidance(&repo);
    assert!(!repo.join(AGENTS_OVERRIDE_PATH).exists());
}

#[test]
fn enhance_deploy_cleans_stale_generated_agents_override() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let override_path = repo.join(AGENTS_OVERRIDE_PATH);
    write(
        &override_path,
        &(serde_json::json!({"maintainer_markdown": legacy_generated_agents_text()}).to_string()
            + "\n"),
    );

    invoke_aethyme(["enhance", "deploy", "--repo", &repo.display().to_string()]).ok();

    assert!(!read(repo.join("AGENTS.md")).contains("## Maintainer Notes"));
    assert_native_root_guidance(&repo);
    assert!(!override_path.exists());
}

#[test]
fn setup_playground_installs_local_generated_artifact_excludes() {
    let script = read(scripts_dir().join("setup-playground.sh"));

    for needle in [
        "write_playground_excludes",
        ".git/info/exclude",
        "AETHYME_PLAYGROUND_GENERATED_ARTIFACTS",
        ".aethyme/",
        ".chau7/",
        ".claude/",
        ".codex/",
        "AGENTS.md",
        "CLAUDE.md",
        "**/AGENTS.md",
        "**/CLAUDE.md",
        "hide_tracked_generated_artifacts",
        "remove_agent_guidance_files",
        "git update-index --skip-worktree",
        "generated_artifacts_are_ignored",
        "git check-ignore --no-index",
    ] {
        assert!(
            script.contains(needle),
            "setup-playground.sh missing {needle:?}"
        );
    }
}

#[test]
fn verify_playground_enforces_guidance_and_discovery_hygiene() {
    let script = read(scripts_dir().join("verify-playground.sh"));

    for needle in [
        "check_root_guidance",
        // Recovery plan P4.5/P4.6: root guidance is checked for the one-call
        // brief and its verification warning; the projection flow is checked
        // on the skill.
        "--format brief",
        "navigation aid, not an answer",
        // Phase 2 template flip (2026-07-30): the staleness check widened
        // from 'src.cli explore' to any executable `-m src.cli` line.
        "executable 'python -m src.cli' guidance",
        "mktemp -t aethyme-explore",
        "top_verification_targets",
        "observability.readiness",
        "verify-targets",
        // Phase 5.5 template flip (2026-08-01): the compact projection is
        // `aethyme explore-summary --from <json>`; no deployed artifact
        // may invoke the Aethyme venv interpreter (comment lines
        // tolerated, same as the Phase 2/3 `-m src.cli` staleness checks).
        "explore-summary --from",
        "check_no_venv_python",
        ".venv/bin/python",
        // Phase 6 (2026-08-01): the check widened from the venv
        // interpreter to ANY Python interpreter — the deployed
        // SessionStart hook's last Python was a bare `python3` heredoc,
        // now `repo hook-envelope`.
        "python3?",
        "has no Python invocation",
        "120 output lines / 20k chars",
        "multi-file `sed`",
        r"navigation_hints\[\]",
        "check_ignored_path",
        "check_no_agent_guidance_files",
        "visible_agent_guidance_files",
        ".aethyme/graph_store.redb",
        ".aethyme/graph",
        ".codex/skills/aethyme/SKILL.md",
        ".claude/skills/aethyme/SKILL.md",
        "AGENTS.md",
        "CLAUDE.md",
        "docs/AGENTS.md",
        "docs/CLAUDE.md",
        "explore.md",
        "graph-task.md",
        "dead-code.md",
        "git status --porcelain --untracked-files=all",
    ] {
        assert!(
            script.contains(needle),
            "verify-playground.sh missing {needle:?}"
        );
    }
}
