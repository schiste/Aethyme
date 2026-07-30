"""Implementation-blind tests for skill deployment and generated onboarding.

Converted from `src.indexing.skills` / `src.indexing.onboarding` imports
to drive the `aethyme` router when those modules were ported to the
aethyme-enhance crate (Phase 3). `repo compile-skills` writes the exact
`expected_onboarding_files` set, so the artifact assertions read the
files it produced.
"""

from __future__ import annotations

import json
from pathlib import Path

from tests.support.cli_invoke import invoke_aethyme


def _compile_skills(repo_path: Path) -> str:
    result = invoke_aethyme(["repo", "compile-skills", str(repo_path)])
    assert result.exit_code == 0, result.output
    return result.output


def _read_artifact(repo_path: Path, relative: str) -> str:
    return (repo_path / relative).read_text(encoding="utf-8")


def test_deploy_skills_installs_only_runtime_navigation_skill(tmp_path: Path) -> None:
    repo_path = tmp_path / "target-repo"
    repo_path.mkdir()

    result = invoke_aethyme(["repo", "deploy-skills", str(repo_path)])

    assert result.exit_code == 0, result.output
    assert "Deployed skills: aethyme" in result.output
    skills_dir = repo_path / ".codex" / "skills"
    assert (skills_dir / "aethyme" / "SKILL.md").is_file()
    assert not (skills_dir / "eval").exists()


def test_deploy_skills_remove_sweeps_deployed_skills(tmp_path: Path) -> None:
    repo_path = tmp_path / "target-repo"
    repo_path.mkdir()
    invoke_aethyme(["repo", "deploy-skills", str(repo_path)])

    result = invoke_aethyme(["repo", "deploy-skills", str(repo_path), "--remove"])

    assert result.exit_code == 0, result.output
    assert "Removed skills: aethyme" in result.output
    assert not (repo_path / ".codex" / "skills" / "aethyme").exists()


def test_compile_skills_renders_repo_specific_artifacts(tmp_path: Path) -> None:
    repo_path = tmp_path / "target-repo"
    repo_path.mkdir()
    (repo_path / "package.json").write_text(
        json.dumps({"name": "demo", "scripts": {"test": "vitest", "dev": "vite"}}),
        encoding="utf-8",
    )
    (repo_path / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    (repo_path / "src").mkdir()
    (repo_path / "src" / "main.ts").write_text("console.log('demo')\n", encoding="utf-8")

    output = _compile_skills(repo_path)

    for relative in (
        ".aethyme/generated/onboarding.json",
        ".aethyme/generated/act-starter.json",
        ".codex/skills/repo-onboarding/SKILL.md",
        ".claude/skills/repo-onboarding/SKILL.md",
        ".codex/skills/repo-act/SKILL.md",
        ".claude/skills/repo-act/SKILL.md",
    ):
        assert f"  compiled   {relative}" in output
        assert (repo_path / relative).is_file()

    artifact = json.loads(_read_artifact(repo_path, ".aethyme/generated/onboarding.json"))
    act_artifact = json.loads(_read_artifact(repo_path, ".aethyme/generated/act-starter.json"))
    assert artifact["repo"]["package_manager"] == "pnpm"
    assert any(cmd["command"] == "pnpm test" for cmd in artifact["commands"])
    assert artifact["primary_commands"]["fast_test"] == "pnpm test"
    assert artifact["primary_commands"]["dev"] == "pnpm dev"
    assert artifact["primary_entrypoints"]["app"]["path"] == "src/main.ts"
    assert any(area["path"] == "src" for area in artifact["areas"])
    assert artifact["summon"]["recommended_when"]
    assert artifact["telemetry"]["counts"]["commands"] >= 1
    assert act_artifact["recommended_mode"] == "repo-act"
    assert act_artifact["commands"]["fast_test"] == "pnpm test"
    assert act_artifact["commands"]["dev"] == "pnpm dev"
    assert act_artifact["primary_entrypoints"]["app"]["path"] == "src/main.ts"
    assert "Repo Onboarding" in _read_artifact(
        repo_path, ".codex/skills/repo-onboarding/SKILL.md"
    )
    assert "## Debugging Checklist" in _read_artifact(
        repo_path, ".codex/skills/repo-act/SKILL.md"
    )


def test_onboarding_overrides_replace_selected_sections(tmp_path: Path) -> None:
    repo_path = tmp_path / "target-repo"
    repo_path.mkdir()
    (repo_path / "pyproject.toml").write_text("[project]\nname = 'demo'\n", encoding="utf-8")
    (repo_path / ".aethyme" / "overrides").mkdir(parents=True)
    (repo_path / ".aethyme" / "overrides" / "onboarding.json").write_text(
        json.dumps(
            {
                "repo": {"kind": "service"},
                "commands": [
                    {
                        "kind": "test",
                        "command": "./scripts/test-fast.sh",
                        "source": "manual-override",
                        "confidence": "high",
                    }
                ],
                "summon": {
                    "recommended_when": ["first touch", "broad task"],
                    "skip_when": ["single known file"],
                },
                "notes": [
                    "Use sandbox credentials from 1Password.",
                    "Do not edit generated files under src/gen directly.",
                ],
            }
        ),
        encoding="utf-8",
    )

    _compile_skills(repo_path)
    artifact = json.loads(_read_artifact(repo_path, ".aethyme/generated/onboarding.json"))

    assert artifact["repo"]["kind"] == "service"
    assert artifact["commands"] == [
        {
            "kind": "test",
            "command": "./scripts/test-fast.sh",
            "source": "manual-override",
            "confidence": "high",
        }
    ]
    assert artifact["summon"]["recommended_when"] == ["first touch", "broad task"]
    assert artifact["notes"] == [
        "Use sandbox credentials from 1Password.",
        "Do not edit generated files under src/gen directly.",
    ]
    assert artifact["telemetry"]["overrides_applied"] is True
    assert artifact["telemetry"]["override_source"] == ".aethyme/overrides/onboarding.json"
    assert artifact["telemetry"]["counts"]["notes"] == 2
    onboarding_skill = _read_artifact(repo_path, ".codex/skills/repo-onboarding/SKILL.md")
    assert "## Maintainer Notes" in onboarding_skill
    assert "Use sandbox credentials from 1Password." in onboarding_skill


def test_onboarding_collects_ranked_primary_commands_from_multiple_sources(tmp_path: Path) -> None:
    repo_path = tmp_path / "target-repo"
    repo_path.mkdir()
    (repo_path / "package.json").write_text(
        json.dumps(
            {
                "name": "demo",
                "scripts": {
                    "dev": "vite",
                    "test": "vitest",
                    "test:e2e": "playwright test",
                    "lint": "eslint .",
                    "build": "vite build",
                },
            }
        ),
        encoding="utf-8",
    )
    (repo_path / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    (repo_path / "Justfile").write_text(
        "install:\n\tpnpm install\n\ntest-fast:\n\tpnpm vitest run\n\nfull-test:\n\tpnpm playwright test\n",
        encoding="utf-8",
    )
    (repo_path / "Procfile").write_text(
        "web: pnpm dev --host 0.0.0.0\nworker: node workers/job.js\n",
        encoding="utf-8",
    )
    (repo_path / "workers").mkdir()
    (repo_path / "workers" / "job.js").write_text("console.log('worker')\n", encoding="utf-8")
    (repo_path / "docker-compose.yml").write_text(
        "services:\n  app:\n    image: node:20\n  test:\n    image: node:20\n",
        encoding="utf-8",
    )

    _compile_skills(repo_path)
    artifact = json.loads(_read_artifact(repo_path, ".aethyme/generated/onboarding.json"))
    act_artifact = json.loads(_read_artifact(repo_path, ".aethyme/generated/act-starter.json"))

    assert artifact["primary_commands"]["install"] == "just install"
    assert artifact["primary_commands"]["dev"] == "pnpm dev"
    assert artifact["primary_commands"]["fast_test"] == "just test-fast"
    assert artifact["primary_commands"]["full_test"] == "just full-test"
    assert artifact["primary_commands"]["lint"] == "pnpm lint"
    assert artifact["primary_commands"]["build"] == "pnpm build"
    assert artifact["primary_entrypoints"]["app"]["path"] == "Procfile:web"
    assert artifact["primary_entrypoints"]["worker"]["path"] == "Procfile:worker"
    assert artifact["primary_entrypoints"]["test"]["path"] in {"docker-compose.yml:test", "package.json:scripts.test"}
    assert any(command["source"] == "Procfile:web" for command in artifact["commands"])
    assert any(command["source"] == "docker-compose.yml" for command in artifact["commands"])
    assert act_artifact["commands"]["fast_test"] == "just test-fast"
    assert act_artifact["commands"]["full_test"] == "just full-test"
    assert act_artifact["commands"]["dev"] == "pnpm dev"
    assert act_artifact["primary_entrypoints"]["app"]["path"] == "Procfile:web"


def test_onboarding_collects_ranked_primary_entrypoints_from_multiple_sources(tmp_path: Path) -> None:
    repo_path = tmp_path / "target-repo"
    repo_path.mkdir()
    (repo_path / "package.json").write_text(
        json.dumps(
            {
                "name": "demo",
                "main": "./src/server.ts",
                "bin": {"demo": "./bin/demo.js"},
                "scripts": {
                    "dev": "node src/server.ts",
                    "worker": "python workers/job.py",
                    "test": "vitest",
                },
            }
        ),
        encoding="utf-8",
    )
    (repo_path / "src").mkdir()
    (repo_path / "src" / "server.ts").write_text("console.log('server')\n", encoding="utf-8")
    (repo_path / "bin").mkdir()
    (repo_path / "bin" / "demo.js").write_text("console.log('cli')\n", encoding="utf-8")
    (repo_path / "workers").mkdir()
    (repo_path / "workers" / "job.py").write_text("print('job')\n", encoding="utf-8")
    (repo_path / "Procfile").write_text(
        "web: node src/server.ts\nworker: python workers/job.py\ntest: pnpm vitest\n",
        encoding="utf-8",
    )

    _compile_skills(repo_path)
    artifact = json.loads(_read_artifact(repo_path, ".aethyme/generated/onboarding.json"))
    act_artifact = json.loads(_read_artifact(repo_path, ".aethyme/generated/act-starter.json"))

    assert artifact["primary_entrypoints"]["app"]["path"] == "src/server.ts"
    assert artifact["primary_entrypoints"]["cli"]["path"] == "bin/demo.js"
    assert artifact["primary_entrypoints"]["worker"]["path"] == "Procfile:worker"
    assert artifact["primary_entrypoints"]["test"]["path"] in {
        "Procfile:test",
        "package.json:scripts.test",
    }
    assert any(entrypoint["path"] == "workers/job.py" for entrypoint in artifact["entrypoints"])
    assert act_artifact["primary_entrypoints"]["cli"]["path"] == "bin/demo.js"
