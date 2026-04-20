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


def aggregate_batch(batch_id: str) -> dict[str, Any]:
    """Return {batch: meta, conditions: {cond: {field: stats}}}."""
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
    return {"batch": meta, "conditions": conditions}
