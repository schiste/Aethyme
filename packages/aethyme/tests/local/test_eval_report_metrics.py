from __future__ import annotations

import json
from pathlib import Path

from src.eval.report import (
    _render_diagnostic_markdown,
    _render_markdown,
    augment_result_with_summary_metrics,
    store_condition_chau7,
    write_eval_run_artifacts,
)


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


def test_harness_health_summary_counts_attribution_and_completion() -> None:
    result = {
        "control-cto-off": {
            "prompt": "prompt",
            "assessment": {"weighted_score": 60.0},
            "run": {
                "input_tokens": 1,
                "attribution_confidence": {
                    "condition": "control-cto-off",
                    "reported_chau7_session_id": "reported",
                    "content_matched_jsonl_path": "/tmp/actual.jsonl",
                    "matched_marker": ".aethyme-eval-output-control-cto-off.json",
                    "attribution_mismatch": True,
                },
                "completion_provenance": {
                    "condition": "control-cto-off",
                    "result_file_seen_at": "2026-05-15T12:00:00+00:00",
                    "transcript_matched_at": "2026-05-15T12:00:01+00:00",
                    "final_collection_source": "result-file",
                },
            },
        },
    }

    augmented = augment_result_with_summary_metrics(result)

    health = augmented["harness_health"]
    assert health["prompt_generation_ok"] is True
    assert health["session_attribution_mismatches"] == 1
    assert health["status_field_trust"] == "degraded"
    assert health["completion_signal"] == "result-file polling"
    assert health["transcripts_content_matched"] == 1
    assert health["result_files_seen"] == 1


def test_report_renders_harness_health_section() -> None:
    result = {
        "task": "x",
        "eval_type": "bug-fix-1",
        "model": {},
        "control-cto-off": {
            "prompt": "prompt",
            "run": {
                "attribution_confidence": {
                    "condition": "control-cto-off",
                    "reported_chau7_session_id": "reported",
                    "content_matched_jsonl_path": "/tmp/actual.jsonl",
                    "attribution_mismatch": True,
                },
                "completion_provenance": {
                    "condition": "control-cto-off",
                    "result_file_seen_at": "2026-05-15T12:00:00+00:00",
                    "final_collection_source": "result-file",
                },
            },
            "assessment": {},
        },
    }

    rendered = _render_markdown(repo_path=Path("/tmp/repo"), result=result)

    assert "## Harness Health" in rendered
    assert "| Session attribution mismatches | 1 |" in rendered
    assert "| Completion signal | result-file polling |" in rendered


def test_diagnostic_report_renders_harness_health_summary() -> None:
    result = {
        "task": "x",
        "eval_type": "dead-code",
        "target": "mediawiki",
        "model": {"name": "haiku"},
        "control-cto-off": {
            "prompt": "prompt",
            "run": {
                "tool_calls": [],
                "attribution_confidence": {
                    "condition": "control-cto-off",
                    "reported_chau7_session_id": "reported",
                    "content_matched_jsonl_path": "/tmp/actual.jsonl",
                    "matched_marker": ".aethyme-eval-output-control-cto-off.json",
                    "attribution_mismatch": True,
                },
                "completion_provenance": {
                    "condition": "control-cto-off",
                    "result_file_seen_at": "2026-05-15T12:00:00+00:00",
                    "transcript_matched_at": "2026-05-15T12:00:01+00:00",
                    "final_collection_source": "result-file",
                },
            },
            "assessment": {"weighted_score": 1.0},
        },
    }

    rendered = _render_diagnostic_markdown(
        repo_path=Path("/tmp/repo"),
        result=result,
        eval_type="dead-code",
    )

    assert "### Harness health summary" in rendered
    assert "### Attribution confidence per condition" in rendered
    assert "| Session attribution mismatches | 1 |" in rendered


def test_condition_observability_artifacts_are_written(tmp_path: Path) -> None:
    result = {
        "control-cto-off": {
            "run": {
                "attribution_confidence": {
                    "condition": "control-cto-off",
                    "reported_chau7_session_id": "reported",
                    "content_matched_jsonl_path": "/tmp/actual.jsonl",
                    "matched_marker": ".aethyme-eval-output-control-cto-off.json",
                    "attribution_mismatch": True,
                },
                "completion_provenance": {
                    "condition": "control-cto-off",
                    "result_file_seen_at": "2026-05-15T12:00:00+00:00",
                    "transcript_matched_at": "2026-05-15T12:00:01+00:00",
                    "final_collection_source": "result-file",
                },
            },
            "assessment": {},
        },
    }

    write_eval_run_artifacts(tmp_path, result)

    attr_path = tmp_path / "conditions/control-cto-off/attribution-confidence.json"
    prov_path = tmp_path / "conditions/control-cto-off/completion-provenance.json"
    assert json.loads(attr_path.read_text())["attribution_mismatch"] is True
    assert json.loads(prov_path.read_text())["final_collection_source"] == "result-file"


def test_store_condition_chau7_persists_observability_artifacts(tmp_path: Path) -> None:
    store_condition_chau7(
        tmp_path,
        "leverage",
        run_id="run-1",
        session_id="expected",
        transcript=[],
        tool_calls=[],
        attribution_confidence={
            "condition": "leverage",
            "reported_chau7_session_id": "reported",
            "content_matched_jsonl_path": "/tmp/leverage.jsonl",
            "attribution_mismatch": True,
        },
        completion_provenance={
            "condition": "leverage",
            "result_file_seen_at": "2026-05-15T12:00:00+00:00",
            "transcript_matched_at": "2026-05-15T12:00:01+00:00",
            "final_collection_source": "result-file",
        },
    )

    assert (tmp_path / "conditions/leverage/attribution-confidence.json").is_file()
    assert (tmp_path / "conditions/leverage/completion-provenance.json").is_file()
