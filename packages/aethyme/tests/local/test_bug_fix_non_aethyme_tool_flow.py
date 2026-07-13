"""End-to-end smoke test for the non-Aethyme tool-context-file flow.

Verifies that prepare_bug_fix_benchmark with tool != aethyme:
  1. Does NOT raise.
  2. Builds a leverage prompt that points at the per-clone tool-
     context file (NOT Aethyme's SKILL.md pointer).
  3. Writes ``.aethyme-eval-tool-context.md`` into the leverage clone.
  4. Skips the Aethyme-specific negative-context plausibility flow
     (status = "skipped_non_aethyme_tool").
  5. Does NOT write the navigation_context.json artifact (it's Aethyme-
     specific and ``None`` for other tools).

The test uses a stub ToolAdapter rather than spawning a real graphify
subprocess — installation is not on the unit-test critical path. The
real install + smoke test for graphify is task 1b, which runs once
locally on the developer's machine, not here.
"""

from __future__ import annotations

import shutil
from pathlib import Path

import pytest


class _StubToolAdapter:
    """Minimal ToolAdapter that returns a fixed leverage addendum.

    Mirrors the structural contract (``.name``, ``.run_condition``)
    without doing any real work. Mocking the adapter rather than the
    underlying subprocess keeps the test independent of pipx state.
    """

    name = "stub-tool"
    display_name = "Stub Tool"

    def __init__(self, addendum: str = "Stub graph report for the task.\n"):
        self._addendum = addendum
        self.calls: list[tuple[str, str]] = []

    def install(self, target_repo: Path) -> None:
        return None

    def implements(self, condition: str) -> bool:
        return condition == "leverage"

    def run_condition(self, condition: str, target_repo: Path, task: str):
        from src.eval.tools import ConditionResult
        self.calls.append((condition, task))
        return ConditionResult(
            prompt_addendum=self._addendum,
            raw_output=self._addendum,
        )


@pytest.fixture
def fake_source_repo(tmp_path: Path) -> Path:
    """Create a minimal git repo that prepare_bug_fix_benchmark can clone.

    The setup function (``setup_bug_fix``) expects specific paths to exist
    inside the repo (rbac-canonical, the failing test, etc.). For the
    purposes of this smoke test we focus on the prepare flow's tool-
    handling — so we'd need either a full Mockup-shaped repo or to make
    setup_bug_fix tolerate missing files. The simplest robust approach
    is to skip when the real Mockup playground isn't available.
    """
    mockup_aethyme = (
        Path.home()
        / "Downloads" / "Repositories" / "Playground" / "Mockup"
        / "Mockup - Aethyme"
    )
    if not mockup_aethyme.is_dir():
        pytest.skip(
            f"Mockup playground not present at {mockup_aethyme} — this "
            "smoke test relies on the real bug-fix setup scaffolding "
            "(plant_bug, create_test, etc.) which expects Mockup's "
            "exact file layout."
        )
    return mockup_aethyme


def test_prepare_non_aethyme_tool_uses_file_pointer_flow(
    fake_source_repo: Path, tmp_path: Path,
) -> None:
    """Full prepare flow with a non-Aethyme tool produces correct artifacts."""
    from src.eval.bug_fix import prepare_bug_fix_benchmark

    stub = _StubToolAdapter(addendum="Stub leverage context: see node X.\n")
    dest = tmp_path / "bench"

    result = prepare_bug_fix_benchmark(
        source=fake_source_repo,
        dest_dir=dest,
        auto_cleanup=False,  # keep the clones around for assertions
        tool=stub,
    )

    # 1. Adapter was actually called for leverage.
    assert any(c[0] == "leverage" for c in stub.calls), (
        f"Adapter.run_condition('leverage', ...) was not invoked; calls={stub.calls}"
    )

    # 2. The tool-context file landed inside the leverage clone.
    leverage_clone = Path(result["repos"]["leverage"])
    tool_context = leverage_clone / ".aethyme-eval-tool-context.md"
    assert tool_context.is_file(), (
        f"Tool-context file not written at {tool_context}. "
        f"Files in leverage clone: {list(leverage_clone.glob('.aethyme-eval-*'))}"
    )
    assert "Stub leverage context" in tool_context.read_text(), (
        "Tool-context file does not contain the adapter's addendum."
    )

    # 3. Leverage prompt mentions the tool by name and points at the file.
    leverage_prompt = result["prompts"]["leverage"]
    assert stub.name in leverage_prompt, (
        f"Leverage prompt should reference tool name {stub.name!r}; "
        f"got: {leverage_prompt[:200]!r}"
    )
    assert ".aethyme-eval-tool-context.md" in leverage_prompt, (
        f"Leverage prompt should point at the tool-context file; "
        f"got: {leverage_prompt[:200]!r}"
    )
    # Aethyme-specific pointer ("SKILL.md") must NOT appear — that
    # would mean the prompt builder didn't pick up the tool branching.
    assert "SKILL.md" not in leverage_prompt, (
        "Leverage prompt for non-Aethyme tool still mentions Aethyme's "
        "SKILL.md; the tool-pointer preamble didn't replace it."
    )

    # 4. Negative-context auto-skipped (status field reflects that).
    assert result["negative_context_status"] == "skipped_non_aethyme_tool"

    # 5. Aethyme-shaped JSON artifacts are absent / null.
    assert result["navigation_context"] is None
    artifact_paths = result["artifacts"]
    assert "navigation_context" not in artifact_paths, (
        "navigation_context artifact should not be written for non-Aethyme tools; "
        f"got: {artifact_paths.get('navigation_context')}"
    )

    # Cleanup
    shutil.rmtree(dest, ignore_errors=True)
