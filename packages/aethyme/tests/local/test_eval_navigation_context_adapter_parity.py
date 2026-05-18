"""Parity test: _build_navigation_context legacy path vs adapter path.

The pure-manifest migration routes Aethyme's leverage/task-conditioned
context-building through the CLI subprocess instead of direct Python
calls into ``build_task_pack`` / ``build_task_context``. Cardinal rule
#2 requires that switching the transport does NOT change the eval's
agent-facing output — the navigation_context.json a leverage-condition
agent reads must be byte-identical whether produced via the legacy
direct-Python path or the new adapter path.

This test enforces that invariant. It runs both paths against the same
target repo + task, normalizes for known-benign serialization details
(JSON round-trip of dict key ordering on Python 3.7+ is insertion-
ordered for both paths), and asserts equality. Any divergence here
blocks the manifest migration from being considered "complete."

The test is skipped if the Mockup playground isn't present, so CI on a
machine without the playground checkout still passes.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

_MOCKUP_AETHYME = (
    Path.home()
    / "Downloads"
    / "Repositories"
    / "Playground"
    / "Mockup"
    / "Mockup - Aethyme"
)
_TASK = (
    "Fix failing test: manage permission does not imply share "
    "in ability-implications.test.ts"
)


@pytest.fixture
def mockup_repo() -> Path:
    if not _MOCKUP_AETHYME.is_dir():
        pytest.skip(
            f"Mockup playground not present at {_MOCKUP_AETHYME} — skipping "
            "parity test. Set up the playground per docs/guides/eval-protocol.md "
            "to enable."
        )
    return _MOCKUP_AETHYME


def test_legacy_and_adapter_paths_produce_identical_context(mockup_repo: Path) -> None:
    """The two transports must produce byte-identical navigation_context dicts."""
    from src.eval.bug_fix import _build_navigation_context
    from src.eval.tools import get_adapter

    adapter = get_adapter("aethyme")

    legacy = _build_navigation_context(mockup_repo, _TASK)
    via_adapter = _build_navigation_context(mockup_repo, _TASK, tool=adapter)

    # Compare via JSON serialization with sorted keys — this catches
    # any value-level divergence while being robust to dict iteration
    # quirks. The eval's prepare flow ultimately writes
    # `json.dumps(nav_context, indent=2)` to a file the agent reads,
    # so JSON-level equality is the contract that matters.
    legacy_json = json.dumps(legacy, sort_keys=True, indent=2)
    adapter_json = json.dumps(via_adapter, sort_keys=True, indent=2)

    if legacy_json != adapter_json:
        # Surface the first divergent top-level key for fast debugging.
        diverged: list[str] = []
        for key in sorted(set(legacy) | set(via_adapter)):
            if legacy.get(key) != via_adapter.get(key):
                diverged.append(key)
        pytest.fail(
            f"Navigation context divergence between legacy and adapter paths.\n"
            f"Divergent top-level keys: {diverged}\n"
            f"Legacy JSON length:  {len(legacy_json)}\n"
            f"Adapter JSON length: {len(adapter_json)}\n"
            f"This blocks the pure-manifest migration — the adapter path "
            f"must produce byte-identical output for tool=aethyme."
        )


def test_non_aethyme_tool_returns_none_on_bug_fix_nav_context(mockup_repo: Path) -> None:
    """Non-Aethyme tools opt out of the structured nav-context JSON.

    Reshape (2026-05-18, Tier 1a): non-Aethyme tools no longer raise
    NotImplementedError. Instead, _build_navigation_context returns
    None — signaling to prepare_bug_fix_benchmark to write the tool's
    leverage output to ``<repo>/.aethyme-eval-tool-context.md`` and
    use a generic tool-pointer preamble in the leverage prompt.

    The previous behavior ("raise") was a temporary guardrail while
    the framework was Aethyme-only. With the file-pointer flow in
    place, returning None is the correct signal — no schema mismatch,
    no silently-empty dict; the caller chooses the per-tool flow.
    """
    from src.eval.bug_fix import _build_navigation_context
    from src.eval.tools import get_adapter

    adapter = get_adapter("graphify")
    result = _build_navigation_context(mockup_repo, _TASK, tool=adapter)
    assert result is None, (
        f"Non-Aethyme tool should return None to signal the tool-context-file "
        f"flow; got {type(result).__name__} instead. If you re-introduce "
        f"structured nav-context support for non-Aethyme tools, update this "
        f"test's contract."
    )
