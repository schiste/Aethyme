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


def test_import_eval_runs_upserts_t_timestamp_dirs_and_summary_metrics(
    tmp_path: Path,
) -> None:
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
                "output": '{"ok":true}',
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
                "top_tools": [
                    {"name": "Read", "count": 2},
                    {"name": "Bash", "count": 1},
                ],
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
    assert row["output"] == '{"ok":true}'


def test_import_eval_runs_preserves_target_and_judge_metrics(tmp_path: Path) -> None:
    db = _load_db_module()
    db.DB_PATH = tmp_path / "evals.db"

    eval_runs_dir = tmp_path / "eval-runs"
    run_dir = eval_runs_dir / "20260420T092820-mediawiki-dead-code"
    run_dir.mkdir(parents=True)

    result = {
        "eval_type": "dead-code",
        "target": "mediawiki",
        "model": {"name": "haiku", "reasoning": "high"},
        "leverage": {
            "prompt": "use aethyme tools",
            "assessment": {"weighted_score": 81.92},
            "run": {
                "input_tokens": 10,
                "output_tokens": 20,
                "cache_read_tokens": 30,
                "cache_create_tokens": 40,
                "cost_usd": 9.4347,
                "duration_seconds": 129.123,
                "num_turns": 111,
                "tool_calls": [{"name": "Grep"}, {"name": "Read"}, {"name": "Grep"}],
                "output": '{"unused_functions":[]}',
                "run_metadata": {
                    "run_id": "run-1776677300-mediawiki-dead-code",
                    "target": "mediawiki",
                },
            },
            "summary_metrics": {
                "quality_score": 81.92,
                "recalculated_eval_score": 118.22,
                "quality_delta_vs_control": 22.55,
                "token_ratio_vs_control": 0.7641,
                "time_ratio_vs_control": 0.973,
                "cost_ratio_vs_control": 0.7611,
                "score_per_1k_tokens": 0.0071,
                "score_per_minute": 38.066,
                "top_tools": [
                    {"name": "Grep", "count": 2},
                    {"name": "Read", "count": 1},
                ],
            },
        },
    }
    (run_dir / "complete-result.json").write_text(json.dumps(result), encoding="utf-8")
    (run_dir / "metadata.json").write_text(
        json.dumps(
            {
                "timestamp": "2026-04-20T09:40:20+00:00",
                "eval_type": "dead-code",
                "repo_path": "/Users/example/Downloads/Repositories/Playground/Mediawiki",
            }
        ),
        encoding="utf-8",
    )
    (run_dir / "events.log").write_text(
        "\n".join(
            [
                "2026-04-20T09:37:36+00:00 [leverage]   sample 1/3: score=0",
                "2026-04-20T09:38:34+00:00 [leverage]   sample 2/3: score=10",
                "2026-04-20T09:39:02+00:00 [leverage]   sample 3/3: score=5",
                "2026-04-20T09:39:02+00:00 [leverage] Judge score: 5.0 (stdev=5.00, reliable=True)",
            ]
        ),
        encoding="utf-8",
    )

    count = db.import_eval_runs(eval_runs_dir)
    assert count == 1

    rows = db.query_results(eval_type="dead-code", target="mediawiki")
    assert len(rows) == 1
    row = rows[0]

    assert row["runId"] == "run-1776677300-mediawiki-dead-code"
    assert row["target"] == "mediawiki"
    assert row["qualityScore"] == 81.92
    assert row["recalculatedEvalScore"] == 118.22
    assert row["judgeScore"] == 5.0
    assert row["judgeStdev"] == 5.0
    assert row["judgeReliable"] is True
    assert row["judgeModel"] == "codex"
    assert row["judgeSamples"] == json.dumps(
        [{"score": 0.0}, {"score": 10.0}, {"score": 5.0}]
    )
    assert row["toolBreakdown"] == json.dumps({"Grep": 2, "Read": 1})
