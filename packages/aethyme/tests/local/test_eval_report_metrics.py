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
    assert augmented["comparison"]["best_quality_condition"] == "explore"
    assert augmented["comparison"]["best_global_condition"] == "control-cto-off"
