from __future__ import annotations

import importlib.util
import json
from pathlib import Path


def _load_db_module():
    db_path = (
        Path(__file__).resolve().parents[4]
        / "packages"
        / "aethyme-eval-ui"
        / "server"
        / "db.py"
    )
    spec = importlib.util.spec_from_file_location("aethyme_eval_ui_db", db_path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_import_eval_runs_upserts_t_timestamp_dirs_and_summary_metrics(tmp_path: Path) -> None:
    db = _load_db_module()
    db.DB_PATH = tmp_path / "evals.db"

    eval_runs_dir = tmp_path / "eval-runs"
    run_dir = eval_runs_dir / "20260415T175219-mediawiki-bug-fix-1"
    run_dir.mkdir(parents=True)

    result = {
        "eval_type": "bug-fix-1",
        "model": {"name": "haiku", "reasoning": "high"},
        "report": {"repo_path": "Mediawiki"},
        "control-cto-off": {
            "prompt": "baseline prompt",
            "assessment": {"weighted_score": 46.17},
            "run": {
                "input_tokens": 100,
                "output_tokens": 200,
                "cache_read_tokens": 300,
                "cache_create_tokens": 400,
                "cost_usd": 1.2345,
                "duration_seconds": 12.5,
                "num_turns": 7,
                "tool_calls": [{"tool": "Read"}, {"tool": "Bash"}, {"tool": "Read"}],
                "output": "{\"ok\":true}",
                "run_metadata": {"run_id": "run-123"},
            },
            "summary_metrics": {
                "quality_score": 46.17,
                "recalculated_eval_score": 100.0,
                "quality_delta_vs_control": 0.0,
                "token_ratio_vs_control": 1.0,
                "time_ratio_vs_control": 1.0,
                "cost_ratio_vs_control": 1.0,
                "score_per_1k_tokens": 46.17,
                "score_per_minute": 221.62,
                "top_tools": [{"name": "Read", "count": 2}, {"name": "Bash", "count": 1}],
            },
        },
    }
    (run_dir / "complete-result.json").write_text(json.dumps(result), encoding="utf-8")
    (run_dir / "metadata.json").write_text(
        json.dumps(
            {
                "timestamp": "2026-04-15T17:52:19+00:00",
                "eval_type": "bug-fix-1",
                "repo_path": "Mediawiki",
            }
        ),
        encoding="utf-8",
    )

    count = db.import_eval_runs(eval_runs_dir)
    assert count == 1

    rows = db.query_results(eval_type="bug-fix-1")
    assert len(rows) == 1
    row = rows[0]

    assert row["runId"] == "run-123"
    assert row["score"] == 46.17
    assert row["qualityScore"] == 46.17
    assert row["recalculatedEvalScore"] == 100.0
    assert row["qualityDeltaVsControl"] == 0.0
    assert row["tokenRatioVsControl"] == 1.0
    assert row["timeRatioVsControl"] == 1.0
    assert row["costRatioVsControl"] == 1.0
    assert row["scorePer1kTokens"] == 46.17
    assert row["scorePerMinute"] == 221.62
    assert row["topTools"] is not None
    assert row["toolBreakdown"] is not None
    assert row["prompt"] == "baseline prompt"
    assert row["output"] == "{\"ok\":true}"
