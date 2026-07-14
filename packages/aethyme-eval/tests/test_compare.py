from __future__ import annotations

from aethyme_eval.baseline import build_baseline
from aethyme_eval.compare import compare_aggregates
from aethyme_eval.models import Aggregate, ConditionKey, MetricSummary


def _summary(value: float) -> MetricSummary:
    return MetricSummary(
        n=1,
        median=value,
        q1=None,
        q3=None,
        iqr=None,
        minimum=value,
        maximum=value,
    )


def _aggregate(
    condition: str,
    *,
    target: str = "grc",
    tokens: float,
    cost: float = 1.0,
    duration: float = 10.0,
    quality: float = 80.0,
) -> Aggregate:
    return Aggregate(
        key=ConditionKey(
            model="haiku",
            target=target,
            eval_type="bug-fix",
            scenario="issue-28",
            condition=condition,
        ),
        n=1,
        run_dirs=("run",),
        first_timestamp=None,
        last_timestamp=None,
        metrics={
            "total_tokens": _summary(tokens),
            "cost_usd": _summary(cost),
            "duration_seconds": _summary(duration),
            "quality_score": _summary(quality),
            "global_score": _summary(quality),
        },
    )


def _baseline(*aggregates: Aggregate) -> dict:
    return {
        "name": "fixture",
        "methodology_hash": "fixture",
        "source_date_range": {"first": None, "last": None},
        "thresholds": {},
        "groups": [aggregate.to_json() for aggregate in aggregates],
    }


def _row_by_condition(payload: dict, condition: str) -> dict:
    for row in payload["rows"]:
        if row["key"]["condition"] == condition:
            return row
    raise AssertionError(f"missing row for {condition}")


def test_compare_fails_large_token_regression_without_quality_gain():
    baseline = _baseline(
        _aggregate("control-cto-off", tokens=100),
        _aggregate("explore", tokens=100),
    )
    current = [
        _aggregate("control-cto-off", tokens=100),
        _aggregate("explore", tokens=160),
    ]

    payload = compare_aggregates(current, baseline)

    row = _row_by_condition(payload, "explore")
    assert row["status"] == "fail"
    assert row["adjusted_token_ratio"] == 1.6
    assert payload["summary"]["fail"] == 1


def test_compare_adjusts_tool_tokens_when_control_drift_is_present():
    baseline = _baseline(
        _aggregate("control-cto-off", tokens=100),
        _aggregate("explore", tokens=100),
    )
    current = [
        _aggregate("control-cto-off", tokens=200),
        _aggregate("explore", tokens=220),
    ]

    payload = compare_aggregates(current, baseline)

    control = _row_by_condition(payload, "control-cto-off")
    explore = _row_by_condition(payload, "explore")
    assert control["status"] == "environment-drift"
    assert explore["status"] == "warn"
    assert explore["token_ratio"] == 2.2
    assert explore["adjusted_token_ratio"] == 1.1
    assert payload["summary"]["fail"] == 0


def test_compare_treats_quality_gain_as_tradeoff_not_failure():
    baseline = _baseline(_aggregate("explore", tokens=100, quality=80))
    current = [_aggregate("explore", tokens=160, quality=86)]

    payload = compare_aggregates(current, baseline)

    row = _row_by_condition(payload, "explore")
    assert row["status"] == "warn"
    assert row["quality_delta"] == 6.0
    assert any("trade-off" in reason for reason in row["reasons"])


def test_build_baseline_excludes_aethyme_target_by_default(tmp_path):
    history = tmp_path / "runs.jsonl"
    history.write_text(
        "\n".join(
            [
                (
                    '{"model":"haiku","target":"aethyme","eval_type":"dead-code",'
                    '"conditions":{"control-cto-off":{"total_tokens":100}}}'
                ),
                (
                    '{"model":"haiku","target":"grc","eval_type":"bug-fix",'
                    '"conditions":{"control-cto-off":{"total_tokens":120}}}'
                ),
            ]
        )
        + "\n",
        encoding="utf-8",
    )

    baseline = build_baseline(history, model="haiku")

    targets = {group["key"]["target"] for group in baseline["groups"]}
    assert targets == {"grc"}
    assert baseline["filters"]["exclude_targets"] == ["aethyme"]
