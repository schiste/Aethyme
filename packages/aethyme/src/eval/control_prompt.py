"""Control-prompt builders for local Aethyme evaluations."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from ..rendering.context_pack import render_prompt_pack


def build_baseline_prompt(repo_path: Path, task: str) -> str:
    """Return the baseline prompt without Aethyme context."""
    return (
        f"Task: {task}\n"
        f"Repository path: {repo_path}\n"
        "Explore the repository directly and produce a structured explanation."
    )


def build_aethyme_prompt(repo_path: Path, task: str, pack: dict[str, Any]) -> str:
    """Return the Aethyme-assisted prompt using the task-context pack."""
    compact_pack = render_prompt_pack(pack)
    return (
        f"Task: {task}\n"
        f"Repository path: {repo_path}\n"
        "Use the provided Aethyme task-context pack as the primary navigation layer.\n"
        "Do not expand beyond the supplied scope unless necessary.\n\n"
        f"{compact_pack}"
    )
