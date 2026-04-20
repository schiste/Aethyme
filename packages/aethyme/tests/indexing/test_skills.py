"""Tests for deploying Aethyme skills into target repositories."""

from __future__ import annotations

from pathlib import Path

from src.indexing.skills import deploy_skills


def test_deploy_skills_installs_only_runtime_navigation_skill(tmp_path: Path) -> None:
    repo_path = tmp_path / "target-repo"
    repo_path.mkdir()

    deployed = deploy_skills(repo_path)

    assert deployed == ["aethyme"]
    skills_dir = repo_path / ".codex" / "skills"
    assert (skills_dir / "aethyme" / "SKILL.md").is_file()
    assert not (skills_dir / "eval").exists()
