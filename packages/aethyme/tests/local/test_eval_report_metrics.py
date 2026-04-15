from __future__ import annotations

from src.eval.report import augment_result_with_summary_metrics


def test_augment_result_with_summary_metrics_adds_global_scores() -> None:
    result = {
        "control-cto-off": {
            "assessment": {"weighted_score": 60.0},
            "run": {
                "cost_usd": 2.0,
                "duration_seconds": 120.0,
                "input_tokens": 1000,
                "output_tokens": 500,
                "cache_read_tokens": 0,
                "cache_create_tokens": 0,
                "tool_calls": [{"tool": "Bash"}, {"tool": "Read"}],
                "structured_output": {"deliverable_status": "success"},
            },
        },
        "explore": {
            "assessment": {"weighted_score": 80.0},
            "run": {
                "cost_usd": 4.0,
                "duration_seconds": 240.0,
                "input_tokens": 4000,
                "output_tokens": 1000,
                "cache_read_tokens": 0,
                "cache_create_tokens": 0,
                "tool_calls": [{"tool": "Bash"}] * 4,
                "structured_output": {"deliverable_status": "success"},
            },
        },
    }

    augmented = augment_result_with_summary_metrics(result)

    control = augmented["control-cto-off"]["summary_metrics"]
    explore = augmented["explore"]["summary_metrics"]

    assert control["quality_score"] == 60.0
    assert control["tool_call_count"] == 2
    assert control["top_tools"][0]["name"] == "Bash"
    assert control["total_tokens"] == 1500
    assert control["score_per_1k_tokens"] == 40.0
    assert control["score_per_minute"] == 30.0
    assert control["global_score"] > explore["global_score"]
    assert control["recalculated_eval_score"] == control["global_score"]
    assert control["quality_delta_vs_control"] == 0.0
    assert control["token_ratio_vs_control"] == 1.0
    assert control["time_ratio_vs_control"] == 1.0
    assert control["cost_ratio_vs_control"] == 1.0
    assert explore["quality_delta_vs_control"] == 20.0
    assert explore["token_ratio_vs_control"] == 0.3
    assert augmented["comparison"]["best_quality_condition"] == "explore"
    assert augmented["comparison"]["best_relative_condition"] == "control-cto-off"
    assert augmented["comparison"]["baseline_condition"] == "control-cto-off"


def test_relative_score_uses_control_quality_as_anchor() -> None:
    result = {
        "control-cto-off": {
            "assessment": {"weighted_score": 50.0},
            "run": {
                "cost_usd": 2.0,
                "duration_seconds": 100.0,
                "input_tokens": 1000,
                "output_tokens": 1000,
                "tool_calls": [{"tool": "Read"}] * 2,
            },
        },
        "task-conditioned": {
            "assessment": {"weighted_score": 55.0},
            "run": {
                "cost_usd": 8.0,
                "duration_seconds": 400.0,
                "input_tokens": 4000,
                "output_tokens": 4000,
                "tool_calls": [{"tool": "Read"}] * 8,
            },
        },
    }

    augmented = augment_result_with_summary_metrics(result)
    control = augmented["control-cto-off"]["summary_metrics"]
    task = augmented["task-conditioned"]["summary_metrics"]

    assert control["global_score"] == 100.0
    assert task["quality_delta_vs_control"] == 5.0
    assert task["token_ratio_vs_control"] == 0.25
    assert task["time_ratio_vs_control"] == 0.25
    assert task["cost_ratio_vs_control"] == 0.25
    assert task["global_score"] < 100.0
