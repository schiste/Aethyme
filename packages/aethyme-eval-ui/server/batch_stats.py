"""Batch-level aggregation for multi-run evals.

A "batch" is a set of eval runs sharing a single batch_id — N sequential
repetitions of the same (eval_type, target, model, scenario, conditions)
request. The reported scorecard for a batch is the *aggregate* across
runs: median + IQR per condition, not individual point estimates.

This module contains two entry points:
    list_batches()           → [batch summary rows]
    aggregate_batch(batch_id)→ {per-condition aggregated stats}

No new tables. Everything is derived from `eval_results` at read time.
"""

from __future__ import annotations

import sqlite3
import statistics
from pathlib import Path
from typing import Any, Iterable


DB_PATH = Path(__file__).resolve().parent / "evals.db"

# Fields we aggregate across runs. Keys are the DB columns; values are the
# output key in the returned dict.
_AGG_FIELDS: dict[str, str] = {
    "quality_score": "quality",
    "judge_score": "judge",
    "judge_stdev": "judge_stdev",
    "recalculated_eval_score": "global_score",
    "total_tokens": "tokens",
    "cost": "cost",
    "turns": "turns",
    "tool_calls": "tools",
    "score_per_1k_tokens": "score_per_1k_tokens",
    "score_per_minute": "score_per_minute",
}


def _quantile_stats(values: list[float]) -> dict[str, float | None]:
    """Return {median, q1, q3, iqr, min, max, n} for a series.

    IQR = Q3 - Q1. For N<4 IQR is None (too few samples for quartiles)."""
    n = len(values)
    if n == 0:
        return {"median": None, "q1": None, "q3": None, "iqr": None,
                "min": None, "max": None, "n": 0}
    vals = sorted(values)
    median = statistics.median(vals)
    if n >= 4:
        q1, _, q3 = statistics.quantiles(vals, n=4, method="inclusive")
        iqr = q3 - q1
    else:
        q1 = q3 = iqr = None
    return {
        "median": round(median, 4),
        "q1": round(q1, 4) if q1 is not None else None,
        "q3": round(q3, 4) if q3 is not None else None,
        "iqr": round(iqr, 4) if iqr is not None else None,
        "min": round(vals[0], 4),
        "max": round(vals[-1], 4),
        "n": n,
    }


def list_batches(limit: int = 50) -> list[dict[str, Any]]:
    """List recent batches with minimal summary metadata."""
    if not DB_PATH.exists():
        return []
    conn = sqlite3.connect(str(DB_PATH))
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(
            """
            SELECT batch_id,
                   MAX(date) AS last_date,
                   MAX(eval_type) AS eval_type,
                   MAX(target) AS target,
                   MAX(model) AS model,
                   MAX(runs_in_batch) AS runs_in_batch,
                   COUNT(DISTINCT run_id) AS distinct_runs,
                   COUNT(*) AS total_rows
            FROM eval_results
            WHERE batch_id IS NOT NULL
            GROUP BY batch_id
            ORDER BY last_date DESC
            LIMIT ?
            """,
            (limit,),
        ).fetchall()
    finally:
        conn.close()
    return [dict(r) for r in rows]


def compare_conditions(
    stats_a: dict[str, float | None],
    stats_b: dict[str, float | None],
    *,
    margin: float = 1.0,
) -> dict[str, Any]:
    """Compare two condition stat blobs and render a verdict.

    Inputs are _quantile_stats() dicts (median + IQR + min/max + n).

    Heuristic, not a formal t-test:
        delta = median_a - median_b
        noise = (iqr_a + iqr_b) / 2   # combined spread proxy
        verdict:
          - "A>B" if delta > noise + margin
          - "B>A" if delta < -(noise + margin)
          - "inconclusive" otherwise

    When IQR is unavailable (N<4 on either side), falls back to half-range
    (max - min) / 2 as a noise proxy. Still heuristic, meant to prevent
    "5 vs 5.1 is better" claims, not to replace statistical testing.
    """
    med_a, med_b = stats_a.get("median"), stats_b.get("median")
    if med_a is None or med_b is None:
        return {"verdict": "no-data", "delta": None, "noise": None,
                "effect_size": None, "margin": margin}

    iqr_a = stats_a.get("iqr")
    iqr_b = stats_b.get("iqr")
    if iqr_a is None:
        rng_a = stats_a.get("max", med_a) - stats_a.get("min", med_a)
        iqr_a = rng_a / 2.0
    if iqr_b is None:
        rng_b = stats_b.get("max", med_b) - stats_b.get("min", med_b)
        iqr_b = rng_b / 2.0

    delta = med_a - med_b
    noise = (iqr_a + iqr_b) / 2.0
    threshold = noise + margin

    if delta > threshold:
        verdict = "A>B"
    elif delta < -threshold:
        verdict = "B>A"
    else:
        verdict = "inconclusive"

    # Effect size normalized by noise — negative = B wins, positive = A wins.
    effect_size = round(delta / noise, 3) if noise > 0 else None

    return {
        "verdict": verdict,
        "delta": round(delta, 3),
        "noise": round(noise, 3),
        "threshold": round(threshold, 3),
        "effect_size": effect_size,
        "margin": margin,
    }


def aggregate_batch(batch_id: str) -> dict[str, Any]:
    """Return {batch: meta, conditions: {cond: {field: stats}}, comparisons: {...}}."""
    if not DB_PATH.exists():
        return {"batch": None, "conditions": {}}
    conn = sqlite3.connect(str(DB_PATH))
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(
            "SELECT * FROM eval_results WHERE batch_id = ? ORDER BY run_index",
            (batch_id,),
        ).fetchall()
    finally:
        conn.close()
    if not rows:
        return {"batch": None, "conditions": {}}

    # Group by condition
    by_cond: dict[str, list[sqlite3.Row]] = {}
    for r in rows:
        by_cond.setdefault(r["condition"], []).append(r)

    # Aggregate per condition
    conditions: dict[str, dict[str, Any]] = {}
    for cond, cond_rows in by_cond.items():
        stats: dict[str, Any] = {}
        for col, out_key in _AGG_FIELDS.items():
            # duration is the odd one out — stored as text. Skip.
            values = [
                float(r[col]) for r in cond_rows
                if r[col] is not None
            ]
            stats[out_key] = _quantile_stats(values)

        # Deliverable success rate
        successes = sum(
            1 for r in cond_rows
            if (r["deliverable_status"] or "success") == "success"
        )
        total = len(cond_rows)
        stats["deliverable_success_rate"] = {
            "successes": successes,
            "total": total,
            "rate": round(successes / total, 4) if total else None,
        }

        # Judge reliability rate (share of runs with reliable=1)
        reliable = sum(1 for r in cond_rows if r["judge_reliable"] == 1)
        has_judge = sum(1 for r in cond_rows if r["judge_reliable"] is not None)
        stats["judge_reliability_rate"] = {
            "reliable": reliable,
            "scored": has_judge,
            "rate": round(reliable / has_judge, 4) if has_judge else None,
        }
        conditions[cond] = stats

    # Batch metadata
    first = rows[0]
    meta = {
        "batch_id": batch_id,
        "eval_type": first["eval_type"],
        "target": first["target"],
        "model": first["model"],
        "reasoning": first["reasoning"],
        "runs_in_batch": first["runs_in_batch"],
        "distinct_runs": len({r["run_id"] for r in rows}),
        "first_date": min(r["date"] for r in rows if r["date"]),
        "last_date": max(r["date"] for r in rows if r["date"]),
        "total_rows": len(rows),
    }

    # Pairwise comparisons against the control baseline. "quality" is the
    # primary metric; if/when pre-registration is added, that metric should
    # determine what we compare on. For now we always compare quality.
    baseline_cond = (
        "control-cto-off" if "control-cto-off" in conditions
        else ("control" if "control" in conditions else next(iter(conditions), None))
    )
    comparisons: dict[str, dict[str, Any]] = {}
    if baseline_cond and baseline_cond in conditions:
        baseline_stats = conditions[baseline_cond].get("quality", {})
        for cond, stats in conditions.items():
            if cond == baseline_cond:
                continue
            comparisons[cond] = {
                "vs": baseline_cond,
                "metric": "quality",
                **compare_conditions(stats.get("quality", {}), baseline_stats),
            }

    discrimination_info = _scenario_discrimination(conditions)

    return {
        "batch": meta,
        "conditions": conditions,
        "comparisons_vs_baseline": comparisons,
        "scenario_discrimination": discrimination_info,
    }


def _scenario_discrimination(
    conditions: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    """How well does this scenario separate conditions on quality?

    Borrows the IRT "discrimination" concept:

        between = stdev of condition median qualities
        within  = mean of per-condition IQRs (noise across runs)
        ratio   = between / within

    Interpretation:
      ratio >= 2.0 : scenario separates conditions strongly
      1.0 <= ratio < 2.0 : usable, meaningful signal
      ratio < 1.0  : low-discrimination — conditions converge into the
                     noise floor, results should be flagged as weak
                     evidence regardless of which condition "wins".

    Requires N>=4 per condition (IQR defined) and at least 2 conditions
    with quality medians. Returns None otherwise.
    """
    medians: list[float] = []
    iqrs: list[float] = []
    for cond, stats in conditions.items():
        q = stats.get("quality", {})
        if not isinstance(q, dict):
            continue
        med = q.get("median")
        iqr = q.get("iqr")
        if med is not None:
            medians.append(float(med))
        # Fall back to half-range when IQR undefined (tiny batches)
        if iqr is None:
            lo, hi = q.get("min"), q.get("max")
            if lo is not None and hi is not None:
                iqr = (float(hi) - float(lo)) / 2.0
        if iqr is not None:
            iqrs.append(float(iqr))

    if len(medians) < 2 or not iqrs:
        return {
            "between_stdev": None, "within_spread": None,
            "ratio": None, "label": "insufficient-data",
            "notes": "Need >=2 conditions with quality medians",
        }

    between = statistics.stdev(medians) if len(medians) >= 2 else 0.0
    within = statistics.mean(iqrs)
    ratio = round(between / within, 3) if within > 0 else None

    if ratio is None:
        label = "insufficient-data"
    elif ratio >= 2.0:
        label = "strong"
    elif ratio >= 1.0:
        label = "usable"
    else:
        label = "low-discrimination"

    return {
        "between_stdev": round(between, 3),
        "within_spread": round(within, 3),
        "ratio": ratio,
        "label": label,
        "notes": (
            "Between-condition spread relative to within-condition noise. "
            "ratio>=2 strong, >=1 usable, <1 low-discrimination (treat results as weak evidence)."
        ),
    }
