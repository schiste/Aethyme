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
    from src.eval.navigation_ctf import _resolve_task_pack
    from src.eval.tools import get_adapter

    legacy_pack = _resolve_task_pack(mockup_repo, _TASK, tool=None)
    adapter_pack = _resolve_task_pack(mockup_repo, _TASK, tool=get_adapter("aethyme"))

    legacy_json = json.dumps(legacy_pack, sort_keys=True)
    adapter_json = json.dumps(adapter_pack, sort_keys=True)

    if legacy_json != adapter_json:
        pytest.fail(
            f"navigation-ctf task_pack divergence between legacy and adapter:\n"
            f"  Legacy bytes:  {len(legacy_json)}\n"
            f"  Adapter bytes: {len(adapter_json)}\n"
        )


def test_navigation_ctf_non_aethyme_tool_raises(mockup_repo: Path) -> None:
    """Non-Aethyme tools must raise NotImplementedError on this eval type."""
    from src.eval.navigation_ctf import _resolve_task_pack
    from src.eval.tools import get_adapter

    with pytest.raises(NotImplementedError, match="task_pack schema"):
        _resolve_task_pack(mockup_repo, _TASK, tool=get_adapter("graphify"))
