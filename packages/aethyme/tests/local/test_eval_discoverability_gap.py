"""Tests for the explore-vs-leverage discoverability gap.

`discoverability_gap_pct = (explore - leverage) / explore * 100`

Positive: pointing the agent at the skill made it cheaper (leverage
beats explore). Negative: the pointer hurt. The gap is computed in
`augment_result_with_summary_metrics` and rendered in the report's
`## Discoverability Gap` section between Model and Scorecard.
"""

from __future__ import annotations

from typing import Any

from src.eval.report import (
    _compute_discoverability_gap,
    _section_discoverability_gap,
    augment_result_with_summary_metrics,
)


def _side(*, cost: float | None = None, total_tokens: int | None = None) -> dict[str, Any]:
    """Synthesize a per-condition result side with the fields the gap
    function reads. `_total_tokens` sums input/output/cache buckets,
    so we put the whole sum into `input_tokens` for simplicity."""
    run: dict[str, Any] = {}
    if cost is not None:
        run["cost_usd"] = cost
    if total_tokens is not None:
        run["input_tokens"] = total_tokens
    return {
        "prompt": "synthetic",
        "run": run,
        "assessment": {"weighted_score": 1.0},
    }


def test_gap_positive_when_leverage_cheaper():
    """Pointing the agent at the skill saved 25% of cost — gap is +25.0%."""
    result = {
        "eval_type": "bug-fix-1",
        "explore": _side(cost=1.00, total_tokens=400_000),
        "leverage": _side(cost=0.75, total_tokens=300_000),
    }
    gap = _compute_discoverability_gap(result)
    assert gap is not None
    assert gap["cost_gap_pct"] == 25.0
    assert gap["tokens_gap_pct"] == 25.0
    assert gap["cost_explore_usd"] == 1.0
    assert gap["cost_leverage_usd"] == 0.75


def test_gap_negative_when_leverage_more_expensive():
    """Pointer over-trusted the tool, ran extra verification, cost more."""
    result = {
        "explore": _side(cost=0.50),
        "leverage": _side(cost=0.60),
    }
    gap = _compute_discoverability_gap(result)
    assert gap is not None
    assert gap["cost_gap_pct"] == -20.0


def test_gap_returns_none_when_either_side_missing():
    assert _compute_discoverability_gap({"explore": _side(cost=1.0)}) is None
    assert _compute_discoverability_gap({"leverage": _side(cost=1.0)}) is None
    assert _compute_discoverability_gap({}) is None


def test_gap_handles_zero_explore_cost_without_dividing_by_zero():
    """Should fall through to tokens — we don't want a NaN gap if
    cost happens to be zero (e.g. local model with no per-call price)."""
    result = {
        "explore": _side(cost=0.0, total_tokens=100_000),
        "leverage": _side(cost=0.0, total_tokens=80_000),
    }
    gap = _compute_discoverability_gap(result)
    assert gap is not None
    assert "cost_gap_pct" not in gap  # zero explore cost → cost gap omitted
    assert gap["tokens_gap_pct"] == 20.0


def test_gap_falls_back_to_tokens_when_cost_unavailable():
    """A run where the agent didn't surface cost still gets a gap on
    tokens — we want the headline metric on every comparable run."""
    result = {
        "explore": _side(total_tokens=200_000),
        "leverage": _side(total_tokens=150_000),
    }
    gap = _compute_discoverability_gap(result)
    assert gap is not None
    assert "cost_gap_pct" not in gap
    assert gap["tokens_gap_pct"] == 25.0


def test_gap_attached_to_comparison_block_after_augment():
    """End-to-end smoke through `augment_result_with_summary_metrics`."""
    result = {
        "eval_type": "bug-fix-1",
        "control-cto-off": _side(cost=2.00, total_tokens=1_000_000),
        "control-cto-on": _side(cost=1.50, total_tokens=750_000),
        "explore": _side(cost=1.00, total_tokens=500_000),
        "leverage": _side(cost=0.80, total_tokens=400_000),
        "task-conditioned": _side(cost=0.70, total_tokens=350_000),
    }
    augment_result_with_summary_metrics(result)
    gap = result["comparison"]["discoverability_gap"]
    assert gap["cost_gap_pct"] == 20.0
    assert gap["tokens_gap_pct"] == 20.0


def test_render_section_skipped_when_gap_absent():
    """Single-condition runs (e.g. explain-repo with explore only)
    must not emit an empty section."""
    lines: list[str] = []
    _section_discoverability_gap(lines, {})
    assert lines == []
    _section_discoverability_gap(lines, {"comparison": {}})
    assert lines == []


def test_render_section_includes_signed_pct_and_dollars():
    lines: list[str] = []
    result = {
        "comparison": {
            "discoverability_gap": {
                "cost_explore_usd": 1.0,
                "cost_leverage_usd": 0.75,
                "cost_gap_pct": 25.0,
                "tokens_explore": 400_000,
                "tokens_leverage": 300_000,
                "tokens_gap_pct": 25.0,
            }
        }
    }
    _section_discoverability_gap(lines, result)
    rendered = "\n".join(lines)
    assert "## Discoverability Gap" in rendered
    assert "+25.00%" in rendered
    assert "$1.0000" in rendered
    assert "300,000" in rendered  # comma-separated tokens


def test_render_section_shows_negative_sign_when_pointer_hurt():
    lines: list[str] = []
    result = {
        "comparison": {
            "discoverability_gap": {
                "cost_explore_usd": 0.5,
                "cost_leverage_usd": 0.6,
                "cost_gap_pct": -20.0,
            }
        }
    }
    _section_discoverability_gap(lines, result)
    rendered = "\n".join(lines)
    # Negative is rendered with the bare minus sign (no leading "+").
    assert "-20.00%" in rendered
    assert "+-20" not in rendered  # no double-sign artifact
