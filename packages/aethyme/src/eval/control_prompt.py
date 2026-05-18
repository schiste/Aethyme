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


def build_leverage_prompt(
    repo_path: Path,
    task: str,
    *,
    tool_name: str | None = None,
    tool_context_relpath: str | None = None,
) -> str:
    """Return the leverage prompt: vanilla task + power-user instruction.

    The agent runs in a playground with the configured tool's skill
    auto-loaded. This prompt nudges it to actively use those tools
    rather than falling back to raw file exploration.

    When ``tool_name`` is None or ``"aethyme"``, the existing Aethyme-
    specific hint is emitted (byte-identical for the default flow).
    When ``tool_name`` is a competitor AND ``tool_context_relpath`` is
    supplied, the prompt names the tool and points at the pre-computed
    tool-context file — structurally identical to the Aethyme variant
    for apples-to-apples comparison.
    """
    is_non_aethyme = tool_name is not None and tool_name != "aethyme"
    if is_non_aethyme and tool_context_relpath:
        return (
            f"Task: {task}\n"
            f"Repository path: {repo_path}\n"
            f"{tool_name} is available in this repository. Pre-computed "
            f"task-specific context is at `{tool_context_relpath}` "
            f"(produced by {tool_name} before this session started). "
            f"Read it if useful; verify its output before acting on it. "
            f"Explore the repository and produce a structured explanation."
        )
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
