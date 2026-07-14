"""Parsers for surviving Aethyme eval run artifacts."""

from __future__ import annotations

import json
import re
from collections.abc import Iterable
from pathlib import Path
from typing import Any

from .constants import CONDITION_ORDER
from .models import ConditionKey, MetricSample


def load_samples(path: Path) -> list[MetricSample]:
    """Load metric samples from a JSONL file, run directory, or tree of runs."""

    path = path.expanduser().resolve()
    if path.is_file():
        if path.name.endswith(".jsonl"):
            return list(load_runs_jsonl(path))
        return list(load_run_json(path))
    if not path.is_dir():
        raise FileNotFoundError(path)

    if (path / "runs.jsonl").exists():
        return list(load_runs_jsonl(path / "runs.jsonl"))
    if (path / "complete-result.json").exists():
        return list(load_run_json(path / "complete-result.json"))

    samples: list[MetricSample] = []
    for child in sorted(path.iterdir()):
        complete_result = child / "complete-result.json"
        if complete_result.exists():
            samples.extend(load_run_json(complete_result))
    return samples


def load_runs_jsonl(path: Path) -> Iterable[MetricSample]:
    with path.open("r", encoding="utf-8") as handle:
        for line in handle:
            if not line.strip():
                continue
            record = json.loads(line)
            yield from _samples_from_history_record(record)


def load_run_json(path: Path) -> Iterable[MetricSample]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    yield from _samples_from_complete_result(payload, path)


def _samples_from_history_record(record: dict[str, Any]) -> Iterable[MetricSample]:
    model = _model_name(record.get("model"))
    target = _target_name(record.get("target") or record.get("repo"), record.get("run_dir"))
    eval_type = str(record.get("eval_type") or "unknown")
    scenario = _blank_to_none(record.get("scenario"))
    run_dir = str(record.get("run_dir") or "")
    timestamp = _blank_to_none(record.get("timestamp"))

    for condition, condition_payload in (record.get("conditions") or {}).items():
        if condition not in CONDITION_ORDER or not isinstance(condition_payload, dict):
            continue
        total_tokens = _int_or_none(condition_payload.get("total_tokens"))
        if not total_tokens or total_tokens <= 0:
            continue
        yield MetricSample(
            key=ConditionKey(
                model=model,
                target=target,
                eval_type=eval_type,
                scenario=scenario,
                condition=condition,
            ),
            run_dir=run_dir,
            timestamp=timestamp,
            quality_score=_float_or_none(
                condition_payload.get("quality_score", condition_payload.get("score"))
            ),
            global_score=_float_or_none(
                condition_payload.get("global_score", condition_payload.get("recalculated_eval_score"))
            ),
            total_tokens=total_tokens,
            cost_usd=_float_or_none(condition_payload.get("cost_usd", condition_payload.get("cost"))),
            duration_seconds=_float_or_none(
                condition_payload.get("duration_s", condition_payload.get("duration_seconds"))
            ),
        )


def _samples_from_complete_result(
    payload: dict[str, Any],
    path: Path,
) -> Iterable[MetricSample]:
    model = _model_name(payload.get("model"))
    target = _target_name(payload.get("target"), str(path.parent))
    eval_type = str(payload.get("eval_type") or "unknown")
    scenario = _blank_to_none(payload.get("scenario"))
    run_dir = str(payload.get("run_id") or path.parent.name)
    timestamp = _blank_to_none(payload.get("timestamp"))

    metadata_path = path.parent / "metadata.json"
    if metadata_path.exists():
        metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
        timestamp = timestamp or _blank_to_none(metadata.get("timestamp"))
        run_dir = str(metadata.get("plan_run_dir") or run_dir)

    for condition in CONDITION_ORDER:
        condition_payload = payload.get(condition)
        if not isinstance(condition_payload, dict):
            continue
        summary = condition_payload.get("summary_metrics") or {}
        run = condition_payload.get("run") or {}
        total_tokens = _int_or_none(summary.get("total_tokens", run.get("total_tokens")))
        if not total_tokens or total_tokens <= 0:
            continue
        yield MetricSample(
            key=ConditionKey(
                model=model,
                target=target,
                eval_type=eval_type,
                scenario=scenario,
                condition=condition,
            ),
            run_dir=run_dir,
            timestamp=timestamp,
            quality_score=_float_or_none(summary.get("quality_score", summary.get("score"))),
            global_score=_float_or_none(
                summary.get("global_score", summary.get("recalculated_eval_score"))
            ),
            total_tokens=total_tokens,
            cost_usd=_float_or_none(summary.get("cost_usd", run.get("cost_usd"))),
            duration_seconds=_float_or_none(
                summary.get("duration_seconds", run.get("duration_seconds"))
            ),
        )


def _model_name(value: Any) -> str:
    if isinstance(value, dict):
        return str(value.get("name") or "unknown")
    if value:
        return str(value)
    return "unknown"


def _target_name(value: Any, run_dir: str | None = None) -> str:
    raw = str(value or "").strip()
    lower = raw.lower()
    run_lower = (run_dir or "").lower()
    if "mediawiki" in lower or "mediawiki" in run_lower:
        return "mediawiki"
    if "grc" in lower or "grc" in run_lower or "mockup" in lower or "mockup" in run_lower:
        return "grc"
    if lower == "aethyme" or lower.endswith("/aethyme"):
        return "aethyme"
    if lower:
        return _slug(lower)
    return "unknown"


def _slug(value: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", value.lower()).strip("-")
    return slug or "unknown"


def _blank_to_none(value: Any) -> str | None:
    if value is None:
        return None
    text = str(value)
    return text if text else None


def _float_or_none(value: Any) -> float | None:
    if value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def _int_or_none(value: Any) -> int | None:
    if value is None:
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None
