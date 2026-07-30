"""Contract checks for playground setup/verification hygiene."""

from __future__ import annotations

import json
from pathlib import Path

from tests.support.cli_invoke import invoke_aethyme

# Frozen contract path (was src.enhance.AGENTS_OVERRIDE_PATH; the Python
# module is deleted — the aethyme-enhance crate owns the constant now).
AGENTS_OVERRIDE_PATH = ".aethyme/overrides/agents.json"

_PACKAGE_ROOT = Path(__file__).resolve().parents[2]
_SCRIPTS_DIR = _PACKAGE_ROOT / "scripts" / "eval"


def _build_repo(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    (root / "package.json").write_text(
        '{"name":"demo","scripts":{"test":"vitest","dev":"vite","build":"vite build"}}\n',
        encoding="utf-8",
    )
    (root / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    (root / "src").mkdir(exist_ok=True)
    (root / "src" / "main.ts").write_text("console.log('demo')\n", encoding="utf-8")


def _legacy_generated_agents_text() -> str:
    return """# Agent Instructions

This repository is **Aethyme-enhanced**. For navigation, caller tracing,
dead-code analysis, or task localization, prefer Aethyme's high-level
Explore surface before brute-force grep.

## Quick start (any agent)

```bash
AETHYME_ROOT="/Users/example/Downloads/Repositories/Aethyme/packages/aethyme"
"$AETHYME_ROOT/.venv/bin/python" -m src.cli explore \\
    --repo "$PWD" --request "<your task>" --format answer-json
```

## Detailed reference

Same content lives at both of these per-product skill paths.

## Verifying this enhancement

```bash
"$AETHYME_ROOT/.venv/bin/python" -m src.cli enhance verify --repo "$PWD"
```
"""


def _assert_native_root_guidance(repo_path: Path) -> None:
    for filename in ("AGENTS.md", "CLAUDE.md"):
        text = (repo_path / filename).read_text(encoding="utf-8")
        assert '"$AETHYME_ROOT/rust/target/release/aethyme" explore' in text
        assert "Do not run `python -m src.cli explore`" in text
        assert '"$AETHYME_ROOT/.venv/bin/python" -m src.cli explore' not in text


def test_enhance_deploy_root_guidance_uses_native_explore(tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    _build_repo(repo_path)

    result = invoke_aethyme(["enhance", "deploy", "--repo", str(repo_path)])
    assert result.exit_code == 0, result.output

    _assert_native_root_guidance(repo_path)


def test_enhance_deploy_does_not_migrate_legacy_generated_agents_as_maintainer(
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo-repo"
    _build_repo(repo_path)
    (repo_path / "AGENTS.md").write_text(
        _legacy_generated_agents_text(),
        encoding="utf-8",
    )

    result = invoke_aethyme(["enhance", "deploy", "--repo", str(repo_path)])
    assert result.exit_code == 0, result.output

    agents_text = (repo_path / "AGENTS.md").read_text(encoding="utf-8")
    assert "## Maintainer Notes" not in agents_text
    _assert_native_root_guidance(repo_path)
    assert not (repo_path / AGENTS_OVERRIDE_PATH).exists()


def test_enhance_deploy_cleans_stale_generated_agents_override(tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    _build_repo(repo_path)
    override_path = repo_path / AGENTS_OVERRIDE_PATH
    override_path.parent.mkdir(parents=True, exist_ok=True)
    override_path.write_text(
        json.dumps({"maintainer_markdown": _legacy_generated_agents_text()}) + "\n",
        encoding="utf-8",
    )

    result = invoke_aethyme(["enhance", "deploy", "--repo", str(repo_path)])
    assert result.exit_code == 0, result.output

    agents_text = (repo_path / "AGENTS.md").read_text(encoding="utf-8")
    assert "## Maintainer Notes" not in agents_text
    _assert_native_root_guidance(repo_path)
    assert not override_path.exists()


def test_setup_playground_installs_local_generated_artifact_excludes() -> None:
    script = (_SCRIPTS_DIR / "setup-playground.sh").read_text(encoding="utf-8")

    assert "write_playground_excludes" in script
    assert ".git/info/exclude" in script
    assert "AETHYME_PLAYGROUND_GENERATED_ARTIFACTS" in script
    for pattern in (
        ".aethyme/",
        ".chau7/",
        ".claude/",
        ".codex/",
        "AGENTS.md",
        "CLAUDE.md",
        "**/AGENTS.md",
        "**/CLAUDE.md",
    ):
        assert pattern in script

    assert "hide_tracked_generated_artifacts" in script
    assert "remove_agent_guidance_files" in script
    assert "git update-index --skip-worktree" in script
    assert "generated_artifacts_are_ignored" in script
    assert "git check-ignore --no-index" in script


def test_verify_playground_enforces_guidance_and_discovery_hygiene() -> None:
    script = (_SCRIPTS_DIR / "verify-playground.sh").read_text(encoding="utf-8")

    assert "check_root_guidance" in script
    assert '"$AETHYME_ROOT/rust/target/release/aethyme" explore' in script
    # Phase 2 template flip (2026-07-30): the staleness check widened from
    # 'src.cli explore' to any executable `-m src.cli` line.
    assert "executable 'python -m src.cli' guidance" in script
    assert "mktemp -t aethyme-explore" in script
    assert "top_verification_targets" in script
    assert "observability.readiness" in script
    assert "verify-targets" in script
    assert "navigation_hints\\[\\]" in script

    assert "check_ignored_path" in script
    assert "check_no_agent_guidance_files" in script
    assert "visible_agent_guidance_files" in script
    for generated_path in (
        ".aethyme/graph_store.redb",
        ".aethyme/graph",
        ".codex/skills/aethyme/SKILL.md",
        ".claude/skills/aethyme/SKILL.md",
        "AGENTS.md",
        "CLAUDE.md",
        "docs/AGENTS.md",
        "docs/CLAUDE.md",
    ):
        assert generated_path in script

    for reference_name in ("explore.md", "graph-task.md", "dead-code.md"):
        assert reference_name in script

    assert "git status --porcelain --untracked-files=all" in script
