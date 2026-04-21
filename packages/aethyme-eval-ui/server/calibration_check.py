"""Judge calibration drift checker.

A calibration item is a hand-scored candidate output stored under
server/calibration/<eval-type>/<id>.json. Periodically we re-score the
items with the current judge and compare against the human anchor score.
Drift beyond a threshold flags the judge as unreliable until investigated.

Intra-rater consistency (judge stdev on the same input) doesn't imply
accuracy — the judge can be stably wrong. This module is the accuracy
probe.

Usage:
    result = run_calibration_check(eval_type="bug-fix-1", tab_directory=...)
    if not result["passes"]:
        warn("judge drifted; treat recent batches as suspect")
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from llm_judge import score_with_judge


CALIBRATION_ROOT = Path(__file__).resolve().parent / "calibration"

# Drift thresholds (per-item and mean-over-set). Tuned to match the
# judge's observed intra-rater stdev (~2–6) plus a small buffer for
# the imprecision of human anchors.
MAX_DRIFT_THRESHOLD = 15.0   # per-item cap
MEAN_DRIFT_THRESHOLD = 10.0  # average across items


def _eval_type_dir(eval_type: str) -> Path:
    # eval_type "bug-fix-1" -> directory "bug-fix-1"
    return CALIBRATION_ROOT / eval_type


def list_calibration_items(eval_type: str) -> list[dict[str, Any]]:
    """Return the calibration items on disk for an eval type.

    Quiet-returns [] when the directory doesn't exist — the check simply
    reports "no items" rather than erroring.
    """
    eval_dir = _eval_type_dir(eval_type)
    if not eval_dir.exists() or not eval_dir.is_dir():
        return []
    items: list[dict[str, Any]] = []
    for path in sorted(eval_dir.glob("*.json")):
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        # Basic schema check — drop obviously-malformed files quietly so
        # a typo in one item doesn't break the whole drift check.
        if (
            isinstance(data, dict)
            and isinstance(data.get("candidate"), str)
            and isinstance(data.get("human_score"), (int, float))
            and 0 <= float(data["human_score"]) <= 100
        ):
            data.setdefault("calibration_id", path.stem)
            items.append(data)
    return items


def run_calibration_check(
    *,
    eval_type: str,
    tab_directory: str,
    aethyme_pkg_path: str,
    samples: int = 3,
    backend: str = "codex",
    log: Any = None,
) -> dict[str, Any]:
    """Score every calibration item with the current judge; report drift.

    `backend` picks which judge CLI to calibrate. The full back-test runs
    the same items through each backend independently and compares mean
    drift — the backend with lower drift against human anchors is the
    more accurate judge for the current agent population.

    Returns:
      {
        eval_type,
        backend,                # echoed so back-test comparisons can key on it
        items_scored, items_found,
        max_drift, mean_drift,
        passes,                 # True when max<=15 AND mean<=10
        threshold_max, threshold_mean,
        per_item: [
          {calibration_id, human_score, judge_mean, drift, stdev, reliable, error}
        ],
      }

    Never raises — a failed calibration item is captured with an `error`
    field rather than aborting the whole check.
    """
    def _log(msg: str) -> None:
        if callable(log):
            log(msg)

    items = list_calibration_items(eval_type)
    if not items:
        return {
            "eval_type": eval_type,
            "backend": backend,
            "items_scored": 0,
            "items_found": 0,
            "max_drift": None,
            "mean_drift": None,
            "passes": False,  # no items = can't certify the judge
            "threshold_max": MAX_DRIFT_THRESHOLD,
            "threshold_mean": MEAN_DRIFT_THRESHOLD,
            "per_item": [],
            "notes": (
                f"No calibration items found under calibration/{eval_type}/. "
                f"Add hand-scored items before running drift checks."
            ),
        }

    # Load reference + task from the schemas module (unless overridden per item).
    import sys
    sys.path.insert(0, aethyme_pkg_path)
    try:
        from src.eval import schemas  # type: ignore
    except Exception as e:
        return {
            "eval_type": eval_type,
            "backend": backend,
            "items_scored": 0,
            "items_found": len(items),
            "max_drift": None,
            "mean_drift": None,
            "passes": False,
            "threshold_max": MAX_DRIFT_THRESHOLD,
            "threshold_mean": MEAN_DRIFT_THRESHOLD,
            "per_item": [],
            "error": f"could not import schemas module: {e}",
        }

    def _lookup(func_prefix: str):
        return (
            getattr(schemas, f"mediawiki_{eval_type.replace('-', '_')}_{func_prefix}", None)
            or getattr(schemas, f"{eval_type.replace('-', '_')}_{func_prefix}", None)
        )
    ref_func = _lookup("reference")
    rubric_func = _lookup("scoring_rubric")

    default_reference = ref_func() if callable(ref_func) else ""
    default_rubric = rubric_func() if callable(rubric_func) else None
    # Get task text from the EVAL_TASKS dict in server/main.py by import —
    # kept here inline to avoid a heavy module import cycle.
    try:
        from main import EVAL_TASKS_STANDALONE as EVAL_TASKS  # future extraction point
    except Exception:
        EVAL_TASKS = {}
    default_task = EVAL_TASKS.get(eval_type, f"Analyze this repository for: {eval_type}")

    per_item: list[dict[str, Any]] = []
    drifts: list[float] = []

    for i, item in enumerate(items, 1):
        calibration_id = item.get("calibration_id", f"item-{i}")
        human_score = float(item["human_score"])
        candidate = item["candidate"]
        task = item.get("task_override") or default_task
        reference = item.get("reference_override") or default_reference

        _log(f"calibration {i}/{len(items)} ({calibration_id}) — human={human_score:.0f}")

        result = score_with_judge(
            task=task,
            reference=reference,
            candidate=candidate,
            rubric=default_rubric,
            samples=samples,
            tab_directory=tab_directory,
            backend=backend,
            log=log,
        )
        judge_mean = result.get("mean_score")
        if not isinstance(judge_mean, (int, float)) or result.get("error"):
            per_item.append({
                "calibration_id": calibration_id,
                "human_score": human_score,
                "judge_mean": None,
                "drift": None,
                "stdev": result.get("stdev"),
                "reliable": result.get("reliable"),
                "error": result.get("error") or "judge returned no score",
            })
            continue

        drift = abs(float(judge_mean) - human_score)
        drifts.append(drift)
        per_item.append({
            "calibration_id": calibration_id,
            "human_score": human_score,
            "judge_mean": float(judge_mean),
            "drift": round(drift, 2),
            "stdev": result.get("stdev"),
            "reliable": result.get("reliable"),
            "error": None,
        })

    max_drift = max(drifts) if drifts else None
    mean_drift = sum(drifts) / len(drifts) if drifts else None
    passes = bool(
        drifts
        and max_drift <= MAX_DRIFT_THRESHOLD
        and mean_drift <= MEAN_DRIFT_THRESHOLD
    )

    return {
        "eval_type": eval_type,
        "backend": backend,
        "items_scored": len(drifts),
        "items_found": len(items),
        "max_drift": round(max_drift, 2) if max_drift is not None else None,
        "mean_drift": round(mean_drift, 2) if mean_drift is not None else None,
        "passes": passes,
        "threshold_max": MAX_DRIFT_THRESHOLD,
        "threshold_mean": MEAN_DRIFT_THRESHOLD,
        "per_item": per_item,
    }


# Minimum drift gap (in score points) for back-test to declare a winner.
# Below this the two backends are considered tied — their accuracy is
# indistinguishable on the current calibration set. Tuned conservatively:
# 2 points is smaller than typical judge intra-rater stdev, so a "tie"
# here really means "the two backends are within noise of each other".
BACKTEST_TIE_THRESHOLD = 2.0


def run_calibration_backtest(
    *,
    eval_type: str,
    tab_directory: str,
    aethyme_pkg_path: str,
    backends: tuple[str, ...] = ("codex", "claude-haiku"),
    samples: int = 3,
    log: Any = None,
) -> dict[str, Any]:
    """Run calibration against each backend in sequence; declare a winner.

    The point of a back-test is to answer a specific question: "given the
    same human-anchored items, does backend A or backend B have lower
    drift?" Lower mean_drift = better-calibrated judge for the current
    agent population.

    Returns:
      {
        eval_type,
        per_backend: {<backend>: <run_calibration_check result>, ...},
        winner: <backend_name> | "tie" | None,
        winner_reason: "<backend> mean_drift=X vs <backend> mean_drift=Y (delta=Z)",
        tie_threshold: BACKTEST_TIE_THRESHOLD,
      }

    A backend that errors or finds no items counts as "no data" and cannot
    win. If both have no data, winner is None.

    Never raises — inherits run_calibration_check's graceful degradation.
    """
    def _log(msg: str) -> None:
        if callable(log):
            log(msg)

    per_backend: dict[str, dict[str, Any]] = {}
    for backend in backends:
        _log(f"[back-test] running calibration with backend={backend}")
        per_backend[backend] = run_calibration_check(
            eval_type=eval_type,
            tab_directory=tab_directory,
            aethyme_pkg_path=aethyme_pkg_path,
            samples=samples,
            backend=backend,
            log=log,
        )

    # Pick the backend with the lowest mean_drift, ignoring backends that
    # failed to score any items.
    scored = {
        b: r for b, r in per_backend.items()
        if r.get("mean_drift") is not None
    }
    if not scored:
        return {
            "eval_type": eval_type,
            "per_backend": per_backend,
            "winner": None,
            "winner_reason": "no backend produced any scored items",
            "tie_threshold": BACKTEST_TIE_THRESHOLD,
        }

    ranked = sorted(scored.items(), key=lambda kv: kv[1]["mean_drift"])
    best_backend, best_result = ranked[0]
    if len(ranked) == 1:
        return {
            "eval_type": eval_type,
            "per_backend": per_backend,
            "winner": best_backend,
            "winner_reason": (
                f"{best_backend} mean_drift={best_result['mean_drift']:.2f} "
                f"(only backend with scored items)"
            ),
            "tie_threshold": BACKTEST_TIE_THRESHOLD,
        }

    second_backend, second_result = ranked[1]
    delta = second_result["mean_drift"] - best_result["mean_drift"]
    if delta < BACKTEST_TIE_THRESHOLD:
        return {
            "eval_type": eval_type,
            "per_backend": per_backend,
            "winner": "tie",
            "winner_reason": (
                f"{best_backend} mean_drift={best_result['mean_drift']:.2f} vs "
                f"{second_backend} mean_drift={second_result['mean_drift']:.2f} "
                f"(delta={delta:.2f} < tie threshold {BACKTEST_TIE_THRESHOLD})"
            ),
            "tie_threshold": BACKTEST_TIE_THRESHOLD,
        }

    return {
        "eval_type": eval_type,
        "per_backend": per_backend,
        "winner": best_backend,
        "winner_reason": (
            f"{best_backend} mean_drift={best_result['mean_drift']:.2f} vs "
            f"{second_backend} mean_drift={second_result['mean_drift']:.2f} "
            f"(delta={delta:.2f})"
        ),
        "tie_threshold": BACKTEST_TIE_THRESHOLD,
    }
