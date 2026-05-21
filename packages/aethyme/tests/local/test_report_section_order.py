"""End-to-end pin on the report's section ordering.

Each section renderer is unit-tested individually, but the
`_render_markdown` orchestration block (the section sequence in
the function body) has no test that fails when the order changes.
This test populates a result dict with every optional section's
inputs and asserts the rendered markdown lists the section
headings in the documented order.

Section ordering matters because:
- Meta and contract come first so the reader sees what the eval
  measures before they look at numbers.
- Discoverability Gap is the headline finding for two-condition
  comparisons; it goes after Harness Health so readers can first
  verify that collection and attribution were trustworthy.
- Agent Policy Notes goes after Tool Call Analysis / Aethyme
  Usage so the reader interprets tool counts before reading the
  orchestrator's commentary on what the agent actually did.

A future refactor that moves any section will trip this test;
update both the docstring of `_render_markdown` and this list
together so they stay in sync.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from src.eval.report import _render_markdown


@pytest.fixture
def populated_result() -> dict[str, Any]:
    """A result dict with every optional section's inputs populated.

    `_render_markdown` calls `augment_result_with_summary_metrics`
    indirectly via the bespoke entry points; we exercise the
    no-augment path here, so we pre-seed the `comparison` block
    that `_section_discoverability_gap` reads from."""
    return {
        "task": "synthetic test task",
        "eval_type": "bug-fix-1",
        "scenario": None,
        "model": {
            "name": "haiku",
            "provider": "anthropic",
            "backend": "claude",
            "reasoning": "default",
        },
        "comparison": {
            "discoverability_gap": {
                "cost_explore_usd": 0.5,
                "cost_leverage_usd": 0.4,
                "cost_gap_pct": 20.0,
            },
        },
        "control-cto-off": {
            "prompt": "p",
            "run": {"cost_usd": 0.5, "input_tokens": 200_000},
            "assessment": {"weighted_score": 90.0},
            "agent_policy_notes": "no aethyme present",
        },
        "explore": {
            "prompt": "p",
            "run": {"cost_usd": 0.5, "input_tokens": 200_000},
            "assessment": {"weighted_score": 90.0},
            "agent_policy_notes": "agent never invoked aethyme",
        },
        "leverage": {
            "prompt": "p",
            "run": {"cost_usd": 0.4, "input_tokens": 150_000},
            "assessment": {"weighted_score": 92.0},
            "agent_policy_notes": "agent ran aethyme once and trusted answer",
        },
        "conditions": ["control-cto-off", "explore", "leverage"],
        "reference_output": {},
        "output_schema": {},
        "scoring_rubric": {},
    }


def test_section_order_matches_documented_sequence(populated_result):
    """Walk the rendered markdown and confirm every documented
    section appears, in order. This is the integration counterpart
    to the per-section unit tests."""
    rendered = _render_markdown(repo_path=Path("/tmp/repo"), result=populated_result)

    # Sections we expect to render with the populated fixture.
    # Score Breakdown / Tool Call Analysis / Aethyme Usage need
    # additional inputs (per-condition score breakdowns, tool_calls,
    # aethyme_usage) to appear; covered by their own unit tests.
    expected_order = [
        "## Meta",
        "## Objective",
        "## Constraints",
        "## Model",
        "## Harness Health",
        "## Discoverability Gap",
        "## Scorecard",
        "## Prompts",
        "## Agent Output",
        "## Agent Policy Notes",
        "## Verdict",
        "## Notes",
        "## Raw Data",
    ]

    positions = []
    for header in expected_order:
        idx = rendered.find(header)
        assert idx != -1, (
            f"missing section {header!r} in rendered report. "
            f"Got headers: {[l for l in rendered.splitlines() if l.startswith('## ')]}"
        )
        positions.append((header, idx))

    # Strictly increasing positions = correct order.
    last_idx = -1
    for header, idx in positions:
        assert idx > last_idx, (
            f"section {header!r} at offset {idx} appears before the "
            f"previous section (offset {last_idx})"
        )
        last_idx = idx


def test_no_duplicate_section_headers(populated_result):
    """A section that renders twice (e.g., from a refactor that
    leaves the old call in place) inflates the report and confuses
    parsers. Exact-match heading count = 1 for each."""
    rendered = _render_markdown(repo_path=Path("/tmp/repo"), result=populated_result)
    headers = [
        line.strip() for line in rendered.splitlines()
        if line.startswith("## ")
    ]
    duplicates = {h for h in headers if headers.count(h) > 1}
    assert not duplicates, f"duplicate section headers in report: {duplicates}"


def test_optional_sections_skipped_when_inputs_absent():
    """A minimal result dict (no comparison, no policy notes)
    must not emit empty Discoverability Gap or Agent Policy Notes
    sections."""
    minimal: dict[str, Any] = {
        "task": "x",
        "eval_type": "bug-fix-1",
        "model": {},
        "control-cto-off": {"prompt": "p", "run": {}, "assessment": {}},
    }
    rendered = _render_markdown(repo_path=Path("/tmp/repo"), result=minimal)
    assert "## Discoverability Gap" not in rendered
    assert "## Agent Policy Notes" not in rendered
    # But the always-on sections still render.
    assert "## Meta" in rendered
    assert "## Verdict" in rendered
