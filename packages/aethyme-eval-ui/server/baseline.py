"""Rolling control-cto-off baseline.

Protocol: `global_score` is defined relative to a control baseline. The
naive approach — use the control row from the same batch — inherits
single-run variance: one lucky or unlucky control run shifts every
other condition's derived score by 5+ points.

Instead, we compute the baseline as the median of the most recent
successful control-cto-off runs matching the key:

    (model, eval_type, target, scenario)

This stabilizes the reference point across runs. New successful control
runs naturally strengthen the baseline over time (no explicit migration,
the median just adapts as rows accumulate).

Key design choices:
 - Median, not mean: robust to outliers
 - Window size K=10: enough for stable median, recent enough to reflect
   current model behavior (default)
 - Filter to deliverable_status IN ('success', NULL) — we don't want a
   failed control run dragging the baseline down
 - Return None when fewer than MIN_ROLLING_SAMPLES runs are available;
   caller falls back to the co-run baseline
"""

from __future__ import annotations

import sqlite3
import statistics
from pathlib import Path
from typing import Any


DB_PATH = Path(__file__).resolve().parent / "evals.db"

# Rolling window of the most recent control runs to include.
DEFAULT_WINDOW = 10

# Below this count of qualifying control runs, don't synthesize a rolling
# baseline — the caller should fall back to the co-run control so we never
# publish a baseline derived from one or two samples.
MIN_ROLLING_SAMPLES = 3


def compute_rolling_baseline(
    *,
    model: str,
    eval_type: str,
    target: str,
    scenario: str | None = None,
    window: int = DEFAULT_WINDOW,
) -> dict[str, Any] | None:
    """Return a baseline dict computed from recent control-cto-off runs.

    Returns None when the window contains fewer than MIN_ROLLING_SAMPLES
    qualifying rows. The caller is expected to fall back to the co-run
    control in that case.

    The returned dict mirrors the shape expected by
    `augment_result_with_summary_metrics(..., baseline_override=...)`:

        {
          "source": "rolling",
          "window_size": <how many rows went in>,
          "quality_score": <median>,
          "total_tokens":  <median>,
          "duration_seconds": <median>,
          "cost_usd": <median>,
        }
    """
    if not DB_PATH.exists():
        return None

    conn = sqlite3.connect(str(DB_PATH))
    conn.row_factory = sqlite3.Row
    try:
        scenario_clause = "scenario IS ?" if scenario is None else "scenario = ?"
        rows = conn.execute(
            f"""
            SELECT quality_score, total_tokens, duration, cost
            FROM eval_results
            WHERE condition = 'control-cto-off'
              AND model = ?
              AND eval_type = ?
              AND target = ?
              AND {scenario_clause}
              AND (deliverable_status IS NULL OR deliverable_status = 'success')
              AND quality_score IS NOT NULL
              AND total_tokens IS NOT NULL AND total_tokens > 0
              AND cost IS NOT NULL AND cost > 0
            ORDER BY date DESC
            LIMIT ?
            """,
            (model, eval_type, target, scenario, window),
        ).fetchall()
    finally:
        conn.close()

    # duration is stored as text ("171s" / "2.3m" / "-") — parse to seconds.
    def _parse_duration(raw: str | None) -> float | None:
        if not raw or raw == "-":
            return None
        raw = raw.strip()
        try:
            if raw.endswith("ms"):
                return float(raw[:-2]) / 1000.0
            if raw.endswith("s"):
                return float(raw[:-1])
            if raw.endswith("m"):
                return float(raw[:-1]) * 60.0
            if raw.endswith("h"):
                return float(raw[:-1]) * 3600.0
            # Bare number = seconds
            return float(raw)
        except ValueError:
            return None

    qualities = [float(r["quality_score"]) for r in rows if r["quality_score"] is not None]
    tokens = [int(r["total_tokens"]) for r in rows if r["total_tokens"]]
    durations = [d for d in (_parse_duration(r["duration"]) for r in rows) if d is not None]
    costs = [float(r["cost"]) for r in rows if r["cost"]]

    # Require the minimum sample count across all axes — we don't publish a
    # partial baseline where tokens has 5 samples but duration has 1.
    usable = min(len(qualities), len(tokens), len(durations), len(costs))
    if usable < MIN_ROLLING_SAMPLES:
        return None

    return {
        "source": "rolling",
        "window_size": usable,
        "quality_score": statistics.median(qualities[:usable]),
        "total_tokens": int(statistics.median(tokens[:usable])),
        "duration_seconds": statistics.median(durations[:usable]),
        "cost_usd": statistics.median(costs[:usable]),
    }
