"""Baseline creation and loading."""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .constants import DEFAULT_BASELINE_NAME, DEFAULT_THRESHOLDS, SELF_TARGETS
from .history import load_samples
from .models import Aggregate
from .stats import aggregate_samples

BASELINE_SCHEMA_VERSION = "aethyme-eval-baseline-v1"


def default_baseline_path() -> Path:
    return Path(__file__).resolve().parents[2] / "baselines" / f"{DEFAULT_BASELINE_NAME}.json"


def build_baseline(
    source_path: Path,
    *,
    name: str = DEFAULT_BASELINE_NAME,
    model: str | None = None,
    exclude_targets: tuple[str, ...] = SELF_TARGETS,
    methodology_hash: str | None = None,
    source_note: str | None = None,
) -> dict[str, Any]:
    samples = load_samples(source_path)
    if model:
        samples = [sample for sample in samples if sample.key.model == model]
    excluded = {target.lower() for target in exclude_targets}
    if excluded:
        samples = [sample for sample in samples if sample.key.target.lower() not in excluded]
    aggregates = aggregate_samples(samples)
    timestamps = sorted(
        timestamp
        for aggregate in aggregates
        for timestamp in (aggregate.first_timestamp, aggregate.last_timestamp)
        if timestamp
    )
    return {
        "schema_version": BASELINE_SCHEMA_VERSION,
        "name": name,
        "generated_at": datetime.now(UTC).isoformat(),
        "source": str(source_path),
        "source_note": source_note,
        "methodology_hash": methodology_hash,
        "filters": {
            key: value
            for key, value in {
                "model": model,
                "exclude_targets": sorted(excluded) if excluded else None,
            }.items()
            if value
        },
        "thresholds": dict(DEFAULT_THRESHOLDS),
        "source_date_range": {
            "first": timestamps[0] if timestamps else None,
            "last": timestamps[-1] if timestamps else None,
        },
        "groups": [aggregate.to_json() for aggregate in aggregates],
    }


def load_baseline(path: Path | None = None) -> dict[str, Any]:
    baseline_path = path or default_baseline_path()
    return json.loads(baseline_path.read_text(encoding="utf-8"))


def write_baseline(payload: dict[str, Any], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def baseline_groups(payload: dict[str, Any]) -> dict[tuple[str, str, str, str | None, str], Aggregate]:
    groups: dict[tuple[str, str, str, str | None, str], Aggregate] = {}
    for item in payload.get("groups", []):
        aggregate = Aggregate.from_json(item)
        key = aggregate.key
        groups[(key.model, key.target, key.eval_type, key.scenario, key.condition)] = aggregate
    return groups
