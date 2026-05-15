"""Tool adapter Protocol — the surface the eval orchestrator consumes.

A tool adapter knows how to:

* install itself into a target repo (for the ``explore`` discoverability test);
* warm any caches before eval start (optional);
* produce per-condition prompt addenda and side-effect artifacts.

Adapters are pure with respect to scoring — they never see the agent's output.
Their job is to set up the eval environment and synthesize the per-condition
prompt context that distinguishes ``explore`` / ``leverage`` /
``task-conditioned`` from each other.

The Protocol uses structural typing so :class:`ManifestToolAdapter` and any
future hand-written adapter (e.g. the in-tree Aethyme adapter) share no
inheritance — they just need to expose the same shape.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Protocol


@dataclass(frozen=True)
class ConditionResult:
    """What an adapter returns from :meth:`ToolAdapter.run_condition`.

    ``prompt_addendum`` is appended to the agent's prompt for this condition.
    ``raw_output`` is the unwrapped command stdout (or artifact file
    contents) — useful when a caller needs to parse structured tool
    output (e.g. ``json.loads(result.raw_output)``) without stripping
    the markdown prompt header. For tools that produce no output at all
    (e.g. ``explore`` with ``strategy=tool_self_install``) this is the
    empty string.
    ``artifact_path`` is the absolute path of any side-effect file the
    adapter dropped on disk (e.g. a graph report). ``None`` when the
    condition produces no on-disk artifact.
    """

    prompt_addendum: str
    raw_output: str = ""
    artifact_path: Path | None = None
    metadata: dict[str, object] | None = None


class ToolAdapter(Protocol):
    """Structural Protocol for an eval tool adapter."""

    name: str
    display_name: str

    def version(self) -> str:
        """Return a version string for run metadata (commit SHA, pkg version, etc.)."""
        ...

    def install(self, target_repo: Path) -> None:
        """Install the tool into / register it with ``target_repo``.

        Called once per condition-clone for the three tool-using conditions
        (``explore``, ``leverage``, ``task-conditioned``). Must be idempotent —
        the orchestrator may invoke it multiple times during prepare.

        Raises :class:`ToolInstallError` on failure; the orchestrator surfaces
        this to the user without continuing.
        """
        ...

    def warm(self, target_repo: Path) -> None:
        """Pre-eval cache priming.  No-op if the tool has nothing to warm."""
        ...

    def implements(self, condition: str) -> bool:
        """``True`` iff this tool supports the given tool-using condition.

        ``condition`` is one of ``"explore"``, ``"leverage"``,
        ``"task-conditioned"``. Conditions the tool doesn't implement are
        skipped in the report (and a note is rendered explaining why).
        """
        ...

    def run_condition(
        self,
        condition: str,
        target_repo: Path,
        task: str,
    ) -> ConditionResult:
        """Produce the prompt addendum (and any side-effect artifact) for ``condition``.

        Called once per (condition, repo-clone, task) tuple during eval prepare.
        For ``explore`` this typically returns an empty addendum — the whole
        point of the explore condition is that the prompt is silent and the
        agent discovers the tool via the deployed skill files.
        """
        ...


class ToolError(Exception):
    """Base for tool-adapter errors."""


class ToolInstallError(ToolError):
    """Raised when an adapter's install / warm step fails."""


class ToolConditionError(ToolError):
    """Raised when an adapter's per-condition command fails."""
