"""Adapter-path smoke test: _build_navigation_context for bug-fix.

History: this file used to enforce byte-identical parity between a
"legacy" direct-Python path (``build_task_pack`` / ``build_task_context``
called in-process) and the new adapter path (CLI subprocess + JSON
parse). The pure-manifest migration deleted the legacy path entirely
in Stage B item 2.3, collapsing ``_build_navigation_context`` to a
single adapter-routed implementation — divergence is no longer
*possible*, so parity comparison no longer has anything to compare.

What remains is a structural smoke test: the surviving adapter path
must produce a dict with the documented top-level shape
(``{mode, repo_path, task, test_file, bug_area, anchors, scope,
file_contents}``) and the correct ``mode`` discriminator. Future
refactors of ``run_condition`` or the JSON-shape contract between
the Aethyme CLI and the Python eval harness will trip this test.

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


_EXPECTED_TOP_LEVEL_KEYS = {
    "mode",
    "repo_path",
    "task",
    "test_file",
    "bug_area",
    "anchors",
    "scope",
    "file_contents",
}


def test_adapter_path_produces_expected_nav_context_shape(mockup_repo: Path) -> None:
    """The single surviving transport must produce the documented dict shape.

    Previously this was a parity test against a legacy direct-Python path.
    That path was removed in Stage B item 2.3 — see this module's docstring
    for history. The smoke test that remains pins the public contract: any
    leverage-condition agent reads ``json.dumps(nav_context)``, so a missing
    or renamed top-level key silently degrades the eval. Catching it here
    is faster than catching it via a regression in scorer output.
    """
    from src.eval._self import self_tool_name
    from src.eval.bug_fix import _build_navigation_context
    from src.eval.tools import get_adapter

    # The self-tool's manifest exercises the structured nav-context flow.
    # Hardcoding "aethyme" would skip this branch under a fork that
    # renames the framework subject via AETHYMEBENCH_SELF_TOOL.
    adapter = get_adapter(self_tool_name())
    nav_context = _build_navigation_context(mockup_repo, _TASK, tool=adapter)

    assert nav_context is not None, (
        "Aethyme adapter must return a dict for bug-fix nav-context. None "
        "is reserved for non-Aethyme tools (see the second test in this file)."
    )

    actual_keys = set(nav_context.keys())
    missing = _EXPECTED_TOP_LEVEL_KEYS - actual_keys
    extra = actual_keys - _EXPECTED_TOP_LEVEL_KEYS
    assert not missing, (
        f"Adapter-path nav-context is missing required top-level keys: "
        f"{sorted(missing)}. If you intentionally removed a key, update "
        f"_EXPECTED_TOP_LEVEL_KEYS above and the docstring of "
        f"_build_navigation_context in src/eval/bug_fix.py."
    )
    assert not extra, (
        f"Adapter-path nav-context has unexpected extra top-level keys: "
        f"{sorted(extra)}. Add them to _EXPECTED_TOP_LEVEL_KEYS if "
        f"intentional, and document them in _build_navigation_context."
    )
    assert nav_context["mode"] == "bug_fix_navigation", (
        f"Expected mode='bug_fix_navigation' (the discriminator agents "
        f"use to select the leverage prompt), got {nav_context['mode']!r}."
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
