"""Control-prompt builders for local Aethyme evaluations.

Design principle: Control and Explore receive **identical** prompts.
The only difference is the runtime environment — Explore runs in a
playground that has the Aethyme skill auto-loaded, Control does not.

Leverage gets the same vanilla prompt **plus** a short "power user"
instruction to actively use Aethyme tools. Task-conditioned assistance
is a separate prompt family that can inject a task pack or engine-
generated context. No temp-file references, no injected CLI commands —
the skill provides those unless the caller intentionally requests a
task-conditioned artifact.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from ..rendering.context_pack import render_prompt_pack


def build_baseline_prompt(repo_path: Path, task: str) -> str:
    """Return the vanilla prompt shared by Control and Explore."""
    return (
        f"Task: {task}\n"
        f"Repository path: {repo_path}\n"
        "Explore the repository and produce a structured explanation."
    )


# Aliases — all three names point at the same vanilla prompt.
build_control_prompt = build_baseline_prompt
build_explore_prompt = build_baseline_prompt


def build_leverage_prompt(repo_path: Path, task: str) -> str:
    """Return the leverage prompt: vanilla task + power-user instruction.

    The agent runs in a playground with the Aethyme skill auto-loaded.
    This prompt nudges it to actively use those tools rather than
    falling back to raw file exploration.
    """
    return (
        f"Task: {task}\n"
        f"Repository path: {repo_path}\n"
        "Use Aethyme tools to navigate the repository graph. "
        "Explore the repository and produce a structured explanation."
    )


def build_aethyme_prompt(repo_path: Path, task: str, pack: dict[str, Any]) -> str:
    """Return the Aethyme-assisted prompt using the task-context pack."""
    compact_pack = render_prompt_pack(pack)
    task_kind = pack.get("task", {}).get("kind")
    if task_kind == "explain_repo":
        return (
            f"Task: {task}\n"
            "Use Aethyme pack only.\n"
            f"{compact_pack}"
        )
    return (
        f"Task: {task}\n"
        f"Repository path: {repo_path}\n"
        "Use the provided Aethyme task-context pack as the primary navigation layer.\n"
        "Do not expand beyond the supplied scope unless necessary.\n\n"
        f"{compact_pack}"
    )


# Legacy alias — kept for backward compat with callers that haven't migrated.
build_iterative_aethyme_prompt = build_leverage_prompt
build_task_conditioned_prompt = build_aethyme_prompt
