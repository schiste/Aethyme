"""Shared constants for the internal eval sentinel."""

from __future__ import annotations

CONDITION_ORDER = (
    "control-cto-off",
    "control-cto-on",
    "explore",
    "leverage",
    "task-conditioned",
)

CONTROL_CONDITION = "control-cto-off"

DEFAULT_BASELINE_NAME = "haiku-2026-05"

SELF_TARGETS = ("aethyme",)

DEFAULT_THRESHOLDS = {
    "warn_token_ratio": 1.25,
    "fail_token_ratio": 1.50,
    "warn_cost_ratio": 1.25,
    "fail_cost_ratio": 1.75,
    "warn_duration_ratio": 1.50,
    "fail_duration_ratio": 2.00,
    "control_drift_token_ratio": 1.25,
    "quality_tradeoff_points": 5.0,
}
