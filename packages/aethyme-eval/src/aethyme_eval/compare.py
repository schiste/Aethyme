"""Compare current eval results against a historical baseline."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .baseline import baseline_groups
from .constants import CONTROL_CONDITION, DEFAULT_THRESHOLDS
from .history import load_samples
from .models import Aggregate, MetricSummary
from .stats import aggregate_samples


@dataclass(frozen=True)
class ComparisonRow:
    key: dict[str, str | None]
    status: str
    reasons: tuple[str, ...]
    current_n: int
    baseline_n: int | None
    token_ratio: float | None
    adjusted_token_ratio: float | None
    cost_ratio: float | None
    duration_ratio: float | None
    quality_delta: float | None
    current: dict[str, Any]
    baseline: dict[str, Any] | None

    def to_json(self) -> dict[str, Any]:
        return {
            "key": self.key,
            "status": self.status,
            "reasons": list(self.reasons),
            "current_n": self.current_n,
            "baseline_n": self.baseline_n,
            "token_ratio": self.token_ratio,
            "adjusted_token_ratio": self.adjusted_token_ratio,
            "cost_ratio": self.cost_ratio,
            "duration_ratio": self.duration_ratio,
            "quality_delta": self.quality_delta,
            "current": self.current,
            "baseline": self.baseline,
        }


def compare_path(results_path: Path, baseline: dict[str, Any]) -> dict[str, Any]:
    samples = load_samples(results_path)
    current = aggregate_samples(samples)
    return compare_aggregates(current, baseline, source=str(results_path))


def compare_aggregates(
    current: list[Aggregate],
    baseline: dict[str, Any],
    *,
    source: str | None = None,
) -> dict[str, Any]:
    thresholds = dict(DEFAULT_THRESHOLDS)
    thresholds.update(baseline.get("thresholds") or {})
    baseline_by_key = baseline_groups(baseline)
    control_ratios = _control_token_ratios(current, baseline_by_key)

    rows = [
        _compare_one(aggregate, baseline_by_key, control_ratios, thresholds)
        for aggregate in sorted(
            current,
            key=lambda item: (
                item.key.model,
                item.key.target,
                item.key.eval_type,
                item.key.scenario or "",
                item.key.condition,
            ),
        )
    ]
    summary = {
        "pass": sum(row.status == "pass" for row in rows),
        "warn": sum(row.status == "warn" for row in rows),
        "environment_drift": sum(row.status == "environment-drift" for row in rows),
        "missing_baseline": sum(row.status == "missing-baseline" for row in rows),
        "fail": sum(row.status == "fail" for row in rows),
        "total": len(rows),
    }
    return {
        "schema_version": "aethyme-eval-comparison-v1",
        "source": source,
        "baseline": {
            "name": baseline.get("name"),
            "methodology_hash": baseline.get("methodology_hash"),
            "source_date_range": baseline.get("source_date_range"),
        },
        "thresholds": thresholds,
        "summary": summary,
        "rows": [row.to_json() for row in rows],
    }


def _compare_one(
    current: Aggregate,
    baseline_by_key: dict[tuple[str, str, str, str | None, str], Aggregate],
    control_ratios: dict[tuple[str, str, str, str | None], float],
    thresholds: dict[str, float],
) -> ComparisonRow:
    key = current.key
    baseline = baseline_by_key.get((key.model, key.target, key.eval_type, key.scenario, key.condition))
    if baseline is None:
        return ComparisonRow(
            key=key.to_json(),
            status="missing-baseline",
            reasons=("No exact baseline for this model/target/eval/scenario/condition.",),
            current_n=current.n,
            baseline_n=None,
            token_ratio=None,
            adjusted_token_ratio=None,
            cost_ratio=None,
            duration_ratio=None,
            quality_delta=None,
            current=_metric_medians(current),
            baseline=None,
        )

    token_ratio = _ratio(current.metrics["total_tokens"], baseline.metrics["total_tokens"])
    cost_ratio = _ratio(current.metrics["cost_usd"], baseline.metrics["cost_usd"])
    duration_ratio = _ratio(current.metrics["duration_seconds"], baseline.metrics["duration_seconds"])
    quality_delta = _delta(current.metrics["quality_score"], baseline.metrics["quality_score"])

    base_key = key.base_key()
    control_ratio = control_ratios.get(base_key)
    adjusted_token_ratio = token_ratio
    reasons: list[str] = []

    if key.condition == CONTROL_CONDITION:
        if token_ratio is not None and token_ratio >= thresholds["control_drift_token_ratio"]:
            reasons.append(
                f"Control tokens rose {token_ratio:.2f}x; environment/model drift suspected."
            )
            status = "environment-drift"
        else:
            status = "pass"
    else:
        if (
            control_ratio is not None
            and control_ratio >= thresholds["control_drift_token_ratio"]
            and token_ratio is not None
        ):
            adjusted_token_ratio = token_ratio / control_ratio
            reasons.append(
                f"Adjusted token ratio by control drift ({control_ratio:.2f}x control)."
            )
        status = _tool_status(
            token_ratio=adjusted_token_ratio,
            raw_token_ratio=token_ratio,
            cost_ratio=cost_ratio,
            duration_ratio=duration_ratio,
            quality_delta=quality_delta,
            thresholds=thresholds,
            reasons=reasons,
        )

    return ComparisonRow(
        key=key.to_json(),
        status=status,
        reasons=tuple(reasons),
        current_n=current.n,
        baseline_n=baseline.n,
        token_ratio=_round(token_ratio),
        adjusted_token_ratio=_round(adjusted_token_ratio),
        cost_ratio=_round(cost_ratio),
        duration_ratio=_round(duration_ratio),
        quality_delta=_round(quality_delta),
        current=_metric_medians(current),
        baseline=_metric_medians(baseline),
    )


def _tool_status(
    *,
    token_ratio: float | None,
    raw_token_ratio: float | None,
    cost_ratio: float | None,
    duration_ratio: float | None,
    quality_delta: float | None,
    thresholds: dict[str, float],
    reasons: list[str],
) -> str:
    quality_tradeoff = quality_delta is not None and quality_delta >= thresholds["quality_tradeoff_points"]
    if token_ratio is not None and token_ratio >= thresholds["fail_token_ratio"] and not quality_tradeoff:
        reasons.append(
            f"Tokens rose {token_ratio:.2f}x without a >= {thresholds['quality_tradeoff_points']:.1f} quality gain."
        )
        return "fail"
    if cost_ratio is not None and cost_ratio >= thresholds["fail_cost_ratio"] and not quality_tradeoff:
        reasons.append(f"Cost rose {cost_ratio:.2f}x without enough quality gain.")
        return "fail"
    if (
        duration_ratio is not None
        and duration_ratio >= thresholds["fail_duration_ratio"]
        and not quality_tradeoff
    ):
        reasons.append(f"Duration rose {duration_ratio:.2f}x without enough quality gain.")
        return "fail"
    warned = False
    if raw_token_ratio is not None and raw_token_ratio >= thresholds["warn_token_ratio"]:
        reasons.append(f"Raw tokens rose {raw_token_ratio:.2f}x.")
        warned = True
    if duration_ratio is not None and duration_ratio >= thresholds["warn_duration_ratio"]:
        reasons.append(f"Duration rose {duration_ratio:.2f}x.")
        warned = True
    if cost_ratio is not None and cost_ratio >= thresholds["warn_cost_ratio"]:
        reasons.append(f"Cost rose {cost_ratio:.2f}x.")
        warned = True
    if quality_tradeoff and warned:
        reasons.append("Quality improved enough to treat this as a trade-off, not an automatic failure.")
    return "warn" if warned else "pass"


def _control_token_ratios(
    current: list[Aggregate],
    baseline_by_key: dict[tuple[str, str, str, str | None, str], Aggregate],
) -> dict[tuple[str, str, str, str | None], float]:
    ratios: dict[tuple[str, str, str, str | None], float] = {}
    for aggregate in current:
        if aggregate.key.condition != CONTROL_CONDITION:
            continue
        baseline = baseline_by_key.get(
            (
                aggregate.key.model,
                aggregate.key.target,
                aggregate.key.eval_type,
                aggregate.key.scenario,
                CONTROL_CONDITION,
            )
        )
        if baseline is None:
            continue
        ratio = _ratio(aggregate.metrics["total_tokens"], baseline.metrics["total_tokens"])
        if ratio is not None:
            ratios[aggregate.key.base_key()] = ratio
    return ratios


def _metric_medians(aggregate: Aggregate) -> dict[str, float | None]:
    return {name: metric.median for name, metric in aggregate.metrics.items()}


def _ratio(current: MetricSummary, baseline: MetricSummary) -> float | None:
    if current.median is None or baseline.median in (None, 0):
        return None
    return float(current.median) / float(baseline.median)


def _delta(current: MetricSummary, baseline: MetricSummary) -> float | None:
    if current.median is None or baseline.median is None:
        return None
    return float(current.median) - float(baseline.median)


def _round(value: float | None) -> float | None:
    return round(value, 4) if value is not None else None
