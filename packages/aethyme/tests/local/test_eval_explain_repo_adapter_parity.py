"""Parity test for explain_repo.py's adapter-vs-legacy task_pack paths.

Same contract as test_eval_navigation_context_adapter_parity.py but for
the explain-repo eval flow. The task_pack feeds
_build_explain_repo_navigation_context which renders the leverage agent's
nav-context.json — byte-identical output across transports is required
to satisfy cardinal rule #2 (no eval output drift).
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
_TASK = "Explain this repo"


@pytest.fixture
def mockup_repo() -> Path:
    if not _MOCKUP_AETHYME.is_dir():
        pytest.skip(f"Mockup playground not present at {_MOCKUP_AETHYME}")
    return _MOCKUP_AETHYME


def test_explain_repo_legacy_and_adapter_paths_match(mockup_repo: Path) -> None:
    """Direct Python build_task_pack must equal adapter-routed result."""
    from src.eval.explain_repo import _resolve_task_pack
    from src.eval.tools import get_adapter

    legacy_pack = _resolve_task_pack(mockup_repo, _TASK, tool=None)
    adapter_pack = _resolve_task_pack(mockup_repo, _TASK, tool=get_adapter("aethyme"))

    legacy_json = json.dumps(legacy_pack, sort_keys=True)
    adapter_json = json.dumps(adapter_pack, sort_keys=True)

    if legacy_json != adapter_json:
        pytest.fail(
            f"explain-repo task_pack divergence between legacy and adapter:\n"
            f"  Legacy bytes:  {len(legacy_json)}\n"
            f"  Adapter bytes: {len(adapter_json)}\n"
        )


def test_explain_repo_non_aethyme_tool_raises(mockup_repo: Path) -> None:
    """Non-Aethyme tools must raise NotImplementedError on this eval type."""
    from src.eval.explain_repo import _resolve_task_pack
    from src.eval.tools import get_adapter

    with pytest.raises(NotImplementedError, match="task_pack schema"):
        _resolve_task_pack(mockup_repo, _TASK, tool=get_adapter("graphify"))
