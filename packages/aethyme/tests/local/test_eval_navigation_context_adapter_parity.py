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


def test_non_aethyme_tool_raises_on_bug_fix_nav_context(mockup_repo: Path) -> None:
    """The bug-fix nav-context schema is Aethyme-specific.

    When a non-Aethyme tool is passed, _build_navigation_context must
    raise NotImplementedError rather than silently producing a dict with
    empty anchors/scope/file_contents that would mislead the leverage
    agent. The fix is to reshape the leverage-prompt construction to
    accept the tool's own prompt_addendum directly; this test pins the
    "raise, don't silently degrade" contract until that reshape lands.
    """
    from src.eval.bug_fix import _build_navigation_context
    from src.eval.tools import get_adapter

    adapter = get_adapter("graphify")

    with pytest.raises(NotImplementedError, match="bug-fix navigation context is Aethyme-specific"):
        _build_navigation_context(mockup_repo, _TASK, tool=adapter)
