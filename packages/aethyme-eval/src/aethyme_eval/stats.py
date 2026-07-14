"""Small-N summary statistics for eval regression checks."""

from __future__ import annotations

import statistics
from collections import defaultdict
from collections.abc import Iterable

from .models import Aggregate, ConditionKey, MetricSample, MetricSummary

METRICS = ("total_tokens", "cost_usd", "duration_seconds", "quality_score", "global_score")


def summarize_values(values: Iterable[float | int | None]) -> MetricSummary:
    clean = sorted(float(value) for value in values if value is not None)
    if not clean:
        return MetricSummary(n=0, median=None, q1=None, q3=None, iqr=None, minimum=None, maximum=None)
    median = statistics.median(clean)
    q1 = q3 = iqr = None
    if len(clean) >= 4:
        quartiles = statistics.quantiles(clean, n=4, method="inclusive")
        q1 = quartiles[0]
        q3 = quartiles[2]
        iqr = q3 - q1
    return MetricSummary(
        n=len(clean),
        median=round(median, 4),
        q1=round(q1, 4) if q1 is not None else None,
        q3=round(q3, 4) if q3 is not None else None,
        iqr=round(iqr, 4) if iqr is not None else None,
        minimum=round(clean[0], 4),
        maximum=round(clean[-1], 4),
    )


def aggregate_samples(samples: Iterable[MetricSample]) -> list[Aggregate]:
    by_key: dict[ConditionKey, list[MetricSample]] = defaultdict(list)
    for sample in samples:
        by_key[sample.key].append(sample)

    aggregates: list[Aggregate] = []
    for key, key_samples in sorted(by_key.items(), key=lambda item: _sortable_key(item[0])):
        timestamps = sorted(sample.timestamp for sample in key_samples if sample.timestamp)
        run_dirs = tuple(sorted({sample.run_dir for sample in key_samples if sample.run_dir}))
        metrics = {
            "total_tokens": summarize_values(sample.total_tokens for sample in key_samples),
            "cost_usd": summarize_values(sample.cost_usd for sample in key_samples),
            "duration_seconds": summarize_values(sample.duration_seconds for sample in key_samples),
            "quality_score": summarize_values(sample.quality_score for sample in key_samples),
            "global_score": summarize_values(sample.global_score for sample in key_samples),
        }
        aggregates.append(
            Aggregate(
                key=key,
                n=len(key_samples),
                run_dirs=run_dirs,
                first_timestamp=timestamps[0] if timestamps else None,
                last_timestamp=timestamps[-1] if timestamps else None,
                metrics=metrics,
            )
        )
    return aggregates


def _sortable_key(key: ConditionKey) -> tuple[str, str, str, str, str]:
    return (key.model, key.target, key.eval_type, key.scenario or "", key.condition)
