"""Adapter-path smoke test: _resolve_task_pack for explain-repo.

History: this file used to enforce byte-identical parity between a
"legacy" direct-Python path (``build_task_pack`` called in-process) and
the new adapter path (CLI subprocess + JSON parse). The pure-manifest
migration deleted the legacy path entirely in Stage B item 2.3,
collapsing ``_resolve_task_pack`` to a single adapter-routed
implementation — divergence is no longer *possible*, so parity
comparison no longer has anything to compare.

What remains is a structural smoke test. Unlike bug-fix's
``_build_navigation_context`` (which owns its dict and can assert an
exact top-level key set), ``_resolve_task_pack`` returns the engine's
``build_task_pack`` output verbatim. That dict is an *intermediate* —
the leverage agent reads the rendered nav-context, not this raw pack —
so the engine is free to add keys. We therefore pin only the
*required subset* that ``run_explain_repo_evaluation`` actually
consumes (``summary`` / ``signals`` / ``anchors``). A missing one of
those silently degrades the eval; an added engine key does not.

The test is skipped if the Mockup playground isn't present, so CI on a
machine without the playground checkout still passes.
"""

from __future__ import annotations

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
        pytest.skip(
            f"Mockup playground not present at {_MOCKUP_AETHYME} — skipping "
            "smoke test. Set up the playground per docs/guides/eval-protocol.md "
            "to enable."
        )
    return _MOCKUP_AETHYME


# Subset of the engine's build_task_pack keys that
# run_explain_repo_evaluation actually reads. The engine may add more;
# losing one of these is what breaks the eval.
_REQUIRED_KEYS = {"summary", "signals", "anchors"}


def test_adapter_path_produces_expected_pack_shape(mockup_repo: Path) -> None:
    """The single surviving transport must produce the consumed pack shape.

    Previously this was a parity test against a legacy direct-Python path.
    That path was removed in Stage B item 2.3 — see this module's docstring
    for history. The smoke test that remains pins the keys
    ``run_explain_repo_evaluation`` reads off the resolved pack, so a
    missing or renamed key is caught here rather than via a regression in
    scorer output.
    """
    from src.eval._self import self_tool_name
    from src.eval.explain_repo import _resolve_task_pack
    from src.eval.tools import get_adapter

    # The self-tool's manifest exercises the Aethyme-shaped pack flow.
    # Hardcoding "aethyme" would skip this branch under a fork that
    # renames the framework subject via AETHYMEBENCH_SELF_TOOL.
    adapter = get_adapter(self_tool_name())
    pack = _resolve_task_pack(mockup_repo, _TASK, tool=adapter)

    assert pack is not None, (
        "Aethyme adapter must return a dict for the explain-repo pack. None "
        "is reserved for non-Aethyme tools (see the second test in this file)."
    )

    missing = _REQUIRED_KEYS - set(pack.keys())
    assert not missing, (
        f"Resolved task_pack is missing required keys consumed by "
        f"run_explain_repo_evaluation: {sorted(missing)}. If the engine "
        f"renamed one, update both the consumer in src/eval/explain_repo.py "
        f"and _REQUIRED_KEYS above."
    )


def test_explain_repo_non_aethyme_tool_returns_none(mockup_repo: Path) -> None:
    """Non-Aethyme tools opt out of the Aethyme-shaped task_pack.

    Reshape (Tier 1a): non-Aethyme tools no longer raise. Returning
    None signals to run_explain_repo_evaluation that it should take
    the tool-context-file flow (write adapter output to .aethyme-eval-
    tool-context.md and use a tool-pointer leverage prompt) instead of
    consuming Aethyme-specific pack.summary / signals / explanation
    rendering.
    """
    from src.eval.explain_repo import _resolve_task_pack
    from src.eval.tools import get_adapter

    result = _resolve_task_pack(mockup_repo, _TASK, tool=get_adapter("graphify"))
    assert result is None
