"""Benchmark repo isolation: one clone per condition.

Creates 5 independent ``git clone --local`` copies of a source repo so each
benchmark condition operates on a pristine copy. Control conditions get no
``.codex/`` directory; explore/leverage/task-conditioned get the Aethyme skill
deployed.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from ..indexing.skills import AETHYME_PACKAGE_ROOT, deploy_skills

CONDITION_NAMES = ("control-cto-off", "control-cto-on", "explore", "leverage", "task-conditioned")
AETHYME_CONDITIONS = frozenset({"explore", "leverage", "task-conditioned"})


def create_condition_repos(
    source: Path,
    dest_dir: Path,
    *,
    aethyme_root: Path | None = None,
    install_deps: bool = True,
) -> dict[str, Path]:
    """Clone *source* once per condition into *dest_dir*.

    Uses ``git clone --local`` for speed (hard links on same filesystem).
    Deploys the Aethyme skill to explore/leverage/task-conditioned clones.
    When *install_deps* is True (default), runs ``pnpm install --frozen-lockfile``
    in each clone so node_modules is available for vitest.

    Returns a mapping of condition name to repo path.
    """
    if aethyme_root is None:
        aethyme_root = AETHYME_PACKAGE_ROOT

    source = Path(source).resolve()
    dest_dir = Path(dest_dir).resolve()
    dest_dir.mkdir(parents=True, exist_ok=True)

    repos: dict[str, Path] = {}
    for cond in CONDITION_NAMES:
        clone_path = dest_dir / cond
        if clone_path.exists():
            raise FileExistsError(
                f"{clone_path} already exists — use a fresh dest or remove it first"
            )

        subprocess.run(
            ["git", "clone", "--local", str(source), str(clone_path)],
            check=True,
            capture_output=True,
        )

        if cond in AETHYME_CONDITIONS:
            deploy_aethyme_skill(clone_path, aethyme_root)

        repos[cond] = clone_path

    # Install node dependencies after all clones are created.
    # Runs in each clone so agents have a working vitest from the start
    # (Codex sandbox blocks network, so agents can't install themselves).
    if install_deps:
        _install_node_deps(repos)

    return repos


def _install_node_deps(repos: dict[str, Path]) -> None:
    """Run ``pnpm install --frozen-lockfile`` in each clone that has a pnpm-lock.yaml."""
    for repo_path in repos.values():
        if not (repo_path / "pnpm-lock.yaml").exists():
            continue
        subprocess.run(
            ["pnpm", "install", "--frozen-lockfile"],
            cwd=str(repo_path),
            check=True,
            capture_output=True,
        )


def deploy_aethyme_skill(repo_path: Path, aethyme_root: Path) -> None:
    """Deploy ``.codex/skills/aethyme/SKILL.md`` into *repo_path*."""
    deploy_skills(repo_path, aethyme_root=aethyme_root, force=True)


def load_condition_repos(dest_dir: Path) -> dict[str, Path]:
    """Load existing condition repos from *dest_dir*.

    Validates that all 5 condition directories exist and are git repos.
    """
    dest_dir = Path(dest_dir).resolve()
    repos: dict[str, Path] = {}
    missing: list[str] = []

    for cond in CONDITION_NAMES:
        clone_path = dest_dir / cond
        if not (clone_path / ".git").exists():
            missing.append(cond)
        else:
            repos[cond] = clone_path

    if missing:
        raise FileNotFoundError(
            f"Missing condition repos in {dest_dir}: {', '.join(missing)}"
        )

    return repos
