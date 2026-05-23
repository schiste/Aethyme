"""Parity test for navigation_ctf.py's adapter-vs-legacy task_pack paths.

Same contract as the bug_fix and explain_repo parity tests. Note: this
eval also calls ``inspect_repository`` and ``graph_expand`` directly —
those are eval-framework infrastructure (reference / anchor-expansion
generation) and stay as Python calls regardless of adapter selection.
Only ``build_task_pack`` is routed through the adapter.
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
    "Find the managing config, owned area, and code entrypoint for "
    "the main runtime path."
)


@pytest.fixture
def mockup_repo() -> Path:
    if not _MOCKUP_AETHYME.is_dir():
        pytest.skip(f"Mockup playground not present at {_MOCKUP_AETHYME}")
    return _MOCKUP_AETHYME


def test_navigation_ctf_legacy_and_adapter_paths_match(mockup_repo: Path) -> None:
    """Direct Python build_task_pack must equal adapter-routed result."""
    from src.eval._self import self_tool_name
    from src.eval.navigation_ctf import _resolve_task_pack
    from src.eval.tools import get_adapter

    legacy_pack = _resolve_task_pack(mockup_repo, _TASK, tool=None)
    # Load the framework's self-tool adapter — that's the one whose
    # task_pack output must remain byte-identical to the legacy path.
    # Hardcoding "aethyme" would skip the parity check under a fork that
    # renames the framework subject via AETHYMEBENCH_SELF_TOOL.
    adapter_pack = _resolve_task_pack(mockup_repo, _TASK, tool=get_adapter(self_tool_name()))

    legacy_json = json.dumps(legacy_pack, sort_keys=True)
    adapter_json = json.dumps(adapter_pack, sort_keys=True)

    if legacy_json != adapter_json:
        pytest.fail(
            f"navigation-ctf task_pack divergence between legacy and adapter:\n"
            f"  Legacy bytes:  {len(legacy_json)}\n"
            f"  Adapter bytes: {len(adapter_json)}\n"
        )


def test_navigation_ctf_non_aethyme_tool_returns_none(mockup_repo: Path) -> None:
    """Non-Aethyme tools opt out of the Aethyme-shaped task_pack.

    Reshape (Tier 1a): non-Aethyme tools no longer raise. Returning
    None tells run_navigation_ctf_evaluation to take the tool-context-
    file flow (write adapter output to .aethyme-eval-tool-context.md
    and use a tool-pointer leverage prompt) instead of consuming
    Aethyme's anchors / scope / file_contents schema.
    """
    from src.eval.navigation_ctf import _resolve_task_pack
    from src.eval.tools import get_adapter

    result = _resolve_task_pack(mockup_repo, _TASK, tool=get_adapter("graphify"))
    assert result is None
