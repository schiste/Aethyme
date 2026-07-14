"""Typed data shapes used by the sentinel.

These are deliberately small dataclasses instead of pydantic models. The
sentinel needs to run in clean worktrees without pulling in the old eval UI
dependency graph.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True, order=True)
class ConditionKey:
    """Identity for one comparable eval arm."""

    model: str
    target: str
    eval_type: str
    scenario: str | None
    condition: str

    def base_key(self) -> tuple[str, str, str, str | None]:
        return (self.model, self.target, self.eval_type, self.scenario)

    def to_json(self) -> dict[str, str | None]:
        return {
            "model": self.model,
            "target": self.target,
            "eval_type": self.eval_type,
            "scenario": self.scenario,
            "condition": self.condition,
        }

    @classmethod
    def from_json(cls, payload: dict[str, Any]) -> ConditionKey:
        return cls(
            model=str(payload["model"]),
            target=str(payload["target"]),
            eval_type=str(payload["eval_type"]),
            scenario=payload.get("scenario"),
            condition=str(payload["condition"]),
        )


@dataclass(frozen=True)
class MetricSample:
    """One condition result from one eval run."""

    key: ConditionKey
    run_dir: str
    timestamp: str | None
    quality_score: float | None
    global_score: float | None
    total_tokens: int | None
    cost_usd: float | None
    duration_seconds: float | None


@dataclass(frozen=True)
class MetricSummary:
    """Robust summary for a metric across runs."""

    n: int
    median: float | None
    q1: float | None
    q3: float | None
    iqr: float | None
    minimum: float | None
    maximum: float | None

    def to_json(self) -> dict[str, int | float | None]:
        return {
            "n": self.n,
            "median": self.median,
            "q1": self.q1,
            "q3": self.q3,
            "iqr": self.iqr,
            "min": self.minimum,
            "max": self.maximum,
        }

    @classmethod
    def from_json(cls, payload: dict[str, Any]) -> MetricSummary:
        return cls(
            n=int(payload.get("n", 0)),
            median=_float_or_none(payload.get("median")),
            q1=_float_or_none(payload.get("q1")),
            q3=_float_or_none(payload.get("q3")),
            iqr=_float_or_none(payload.get("iqr")),
            minimum=_float_or_none(payload.get("min")),
            maximum=_float_or_none(payload.get("max")),
        )


@dataclass(frozen=True)
class Aggregate:
    """All samples for one comparable eval arm."""

    key: ConditionKey
    n: int
    run_dirs: tuple[str, ...]
    first_timestamp: str | None
    last_timestamp: str | None
    metrics: dict[str, MetricSummary]

    def to_json(self) -> dict[str, Any]:
        return {
            "key": self.key.to_json(),
            "n": self.n,
            "run_dirs": list(self.run_dirs),
            "first_timestamp": self.first_timestamp,
            "last_timestamp": self.last_timestamp,
            "metrics": {name: metric.to_json() for name, metric in self.metrics.items()},
        }

    @classmethod
    def from_json(cls, payload: dict[str, Any]) -> Aggregate:
        return cls(
            key=ConditionKey.from_json(payload["key"]),
            n=int(payload["n"]),
            run_dirs=tuple(str(item) for item in payload.get("run_dirs", [])),
            first_timestamp=payload.get("first_timestamp"),
            last_timestamp=payload.get("last_timestamp"),
            metrics={
                str(name): MetricSummary.from_json(metric)
                for name, metric in payload.get("metrics", {}).items()
            },
        )


def _float_or_none(value: Any) -> float | None:
    if value is None:
        return None
    return float(value)
