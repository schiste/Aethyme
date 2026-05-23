"""Inline-warm helpers for the eval build-inputs phase.

The eval pipeline has two warm paths:

1. The orchestrator's ``warm`` phase, which runs after ``build-inputs`` and
   handles tools whose state lives in process-level caches (Aethyme daemon).
2. *This* inline path, used during ``build-inputs`` by tools whose state
   lives in per-repo on-disk directories (graphify's ``graphify-out/``).
   The leverage query needs that state populated before the addendum can
   be generated; the orchestrator's later warm phase is too late.

Both helpers below take a ``log_prefix`` so the existing stderr lines
("[bug_fix.prepare] graphify warm() failed on …") stay byte-identical
across the refactor — the parity tests in 1.10 will be the long-term
gate on that.

Failures are non-fatal by design. A warm or query error becomes either a
silent stderr log (warm) or an HTML-comment addendum (query); the eval
still runs and the agent just doesn't get leverage context. That tracks
real production failure modes — a stale or broken tool shouldn't tank an
otherwise-valid measurement.
"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path
from typing import Iterable

from .tools.base import ToolAdapter


def warm_and_query_leverage(
    tool: ToolAdapter,
    repo_path: Path,
    task: str,
    *,
    tool_context_path: Path,
    log_prefix: str,
) -> str:
    """Warm *tool* against *repo_path*, query the leverage condition, write
    the addendum to *tool_context_path*, and return the same addendum string.

    The return value lets a caller fan the addendum out to additional paths
    (bug_fix.py writes to both the leverage and negative-context clones
    using one query). Single-path callers (explain_repo, navigation_ctf)
    can ignore the return.

    *log_prefix* is the literal string between the brackets of the stderr
    log line, e.g. ``"bug_fix.prepare"`` produces
    ``[bug_fix.prepare] graphify warm() failed on …``.
    """
    try:
        tool.warm(repo_path)
    except Exception as exc:  # noqa: BLE001 — warm is best-effort
        print(
            f"[{log_prefix}] {tool.name} warm() failed on "
            f"{repo_path}: {type(exc).__name__}: {exc}",
            file=sys.stderr,
        )

    try:
        leverage_result = tool.run_condition("leverage", repo_path, task)
        addendum = leverage_result.raw_output or leverage_result.prompt_addendum or ""
    except Exception as exc:  # noqa: BLE001 — degrade gracefully
        addendum = (
            f"<!-- tool '{tool.name}' leverage condition errored: "
            f"{type(exc).__name__}: {exc} -->\n"
        )

    tool_context_path.write_text(addendum, encoding="utf-8")
    return addendum


def fanout_tool_state(
    source_repo: Path,
    target_repos: Iterable[Path],
    *,
    candidate_state_dirs: tuple[str, ...] = ("graphify-out", ".graphify"),
    log_prefix: str,
) -> None:
    """Copy the tool's per-repo state directory from *source_repo* to each
    of *target_repos*.

    Detection scans *candidate_state_dirs* in order; the first that exists
    in *source_repo* is the one fanned out. If none exist (e.g. the warm
    step failed and produced no on-disk state) this is a no-op.

    We copy rather than re-warm because (a) graphify-out can be large and
    re-running graphify per clone is the dominant cost of prepare, and
    (b) copying guarantees bit-identical state across conditions so any
    eval variance can't be blamed on tool nondeterminism.

    Per-target copy errors are logged to stderr but don't abort — same
    rationale as :func:`warm_and_query_leverage`.
    """
    actual_state_dir: str | None = None
    for cand in candidate_state_dirs:
        if (source_repo / cand).is_dir():
            actual_state_dir = cand
            break
    if actual_state_dir is None:
        return

    src = source_repo / actual_state_dir
    for target in target_repos:
        dst = target / actual_state_dir
        if dst.exists():
            shutil.rmtree(dst)
        try:
            shutil.copytree(src, dst)
        except Exception as exc:  # noqa: BLE001
            print(
                f"[{log_prefix}] failed to copy {actual_state_dir} "
                f"from {source_repo.name} to {target.name}: "
                f"{type(exc).__name__}: {exc}",
                file=sys.stderr,
            )
