"""Tests for deploying Aethyme skills into target repositories."""

from __future__ import annotations

import json
from pathlib import Path

from src.indexing.onboarding import expected_onboarding_files
from src.indexing.skills import deploy_skills


def test_deploy_skills_installs_only_runtime_navigation_skill(tmp_path: Path) -> None:
    repo_path = tmp_path / "target-repo"
    repo_path.mkdir()

    deployed = deploy_skills(repo_path)

    assert deployed == ["aethyme"]
    skills_dir = repo_path / ".codex" / "skills"
    assert (skills_dir / "aethyme" / "SKILL.md").is_file()
    assert not (skills_dir / "eval").exists()


def test_expected_onboarding_files_render_repo_specific_artifacts(tmp_path: Path) -> None:
    repo_path = tmp_path / "target-repo"
    repo_path.mkdir()
    (repo_path / "package.json").write_text(
        json.dumps({"name": "demo", "scripts": {"test": "vitest", "dev": "vite"}}),
        encoding="utf-8",
    )
    (repo_path / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    (repo_path / "src").mkdir()
    (repo_path / "src" / "main.ts").write_text("console.log('demo')\n", encoding="utf-8")

    files = expected_onboarding_files(repo_path)

    assert ".aethyme/generated/onboarding.json" in files
    assert ".aethyme/generated/act-starter.json" in files
    assert ".codex/skills/repo-onboarding/SKILL.md" in files
    assert ".claude/skills/repo-onboarding/SKILL.md" in files
    assert ".codex/skills/repo-act/SKILL.md" in files
    assert ".claude/skills/repo-act/SKILL.md" in files

    artifact = json.loads(files[".aethyme/generated/onboarding.json"])
    act_artifact = json.loads(files[".aethyme/generated/act-starter.json"])
    assert artifact["repo"]["package_manager"] == "pnpm"
    assert any(cmd["command"] == "pnpm test" for cmd in artifact["commands"])
    assert any(area["path"] == "src" for area in artifact["areas"])
    assert artifact["summon"]["recommended_when"]
    assert artifact["telemetry"]["counts"]["commands"] >= 1
    assert act_artifact["recommended_mode"] == "repo-act"
    assert act_artifact["commands"]["fast_test"] == "pnpm test"
    assert "Repo Onboarding" in files[".codex/skills/repo-onboarding/SKILL.md"]
    assert "## Debugging Checklist" in files[".codex/skills/repo-act/SKILL.md"]


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

    files = expected_onboarding_files(repo_path)
    artifact = json.loads(files[".aethyme/generated/onboarding.json"])

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
    assert "## Maintainer Notes" in files[".codex/skills/repo-onboarding/SKILL.md"]
    assert "Use sandbox credentials from 1Password." in files[".codex/skills/repo-onboarding/SKILL.md"]
