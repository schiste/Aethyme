from __future__ import annotations

import json

from aethyme_eval.cli import main


def test_compare_cli_returns_2_when_fail_on_regression_is_set(tmp_path):
    baseline = tmp_path / "baseline.json"
    baseline.write_text(
        json.dumps(
            {
                "name": "fixture",
                "methodology_hash": "fixture",
                "source_date_range": {"first": None, "last": None},
                "thresholds": {},
                "groups": [
                    {
                        "key": {
                            "model": "haiku",
                            "target": "grc",
                            "eval_type": "bug-fix",
                            "scenario": None,
                            "condition": "explore",
                        },
                        "n": 1,
                        "run_dirs": ["baseline"],
                        "first_timestamp": None,
                        "last_timestamp": None,
                        "metrics": {
                            "total_tokens": {
                                "n": 1,
                                "median": 100,
                                "q1": None,
                                "q3": None,
                                "iqr": None,
                                "min": 100,
                                "max": 100,
                            },
                            "cost_usd": {
                                "n": 1,
                                "median": 1,
                                "q1": None,
                                "q3": None,
                                "iqr": None,
                                "min": 1,
                                "max": 1,
                            },
                            "duration_seconds": {
                                "n": 1,
                                "median": 10,
                                "q1": None,
                                "q3": None,
                                "iqr": None,
                                "min": 10,
                                "max": 10,
                            },
                            "quality_score": {
                                "n": 1,
                                "median": 80,
                                "q1": None,
                                "q3": None,
                                "iqr": None,
                                "min": 80,
                                "max": 80,
                            },
                            "global_score": {
                                "n": 1,
                                "median": 80,
                                "q1": None,
                                "q3": None,
                                "iqr": None,
                                "min": 80,
                                "max": 80,
                            },
                        },
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    run_dir = tmp_path / "20260521T121950-grc-bug-fix-haiku"
    run_dir.mkdir()
    (run_dir / "complete-result.json").write_text(
        json.dumps(
            {
                "model": "haiku",
                "target": "grc",
                "eval_type": "bug-fix",
                "explore": {
                    "summary_metrics": {
                        "total_tokens": 180,
                        "cost_usd": 1,
                        "duration_seconds": 10,
                        "quality_score": 80,
                        "global_score": 80,
                    }
                },
            }
        ),
        encoding="utf-8",
    )

    code = main(
        [
            "compare",
            str(run_dir),
            "--baseline",
            str(baseline),
            "--format",
            "json",
            "--fail-on-regression",
        ]
    )

    assert code == 2


def test_compare_cli_returns_2_when_baseline_is_missing(tmp_path):
    baseline = tmp_path / "baseline.json"
    baseline.write_text(
        json.dumps({"name": "fixture", "thresholds": {}, "groups": []}),
        encoding="utf-8",
    )
    run_dir = tmp_path / "20260521T121950-new-playground-bug-fix-haiku"
    run_dir.mkdir()
    (run_dir / "complete-result.json").write_text(
        json.dumps(
            {
                "model": "haiku",
                "target": "new-playground",
                "eval_type": "bug-fix",
                "explore": {
                    "summary_metrics": {
                        "total_tokens": 100,
                        "cost_usd": 1,
                        "duration_seconds": 10,
                        "quality_score": 80,
                    }
                },
            }
        ),
        encoding="utf-8",
    )

    code = main(
        [
            "compare",
            str(run_dir),
            "--baseline",
            str(baseline),
            "--fail-on-regression",
        ]
    )

    assert code == 2


def test_compare_cli_returns_2_when_there_are_no_comparable_rows(tmp_path):
    baseline = tmp_path / "baseline.json"
    baseline.write_text(
        json.dumps({"name": "fixture", "thresholds": {}, "groups": []}),
        encoding="utf-8",
    )
    empty_results = tmp_path / "empty-results"
    empty_results.mkdir()

    code = main(
        [
            "compare",
            str(empty_results),
            "--baseline",
            str(baseline),
            "--fail-on-regression",
        ]
    )

    assert code == 2


def test_playground_command_uses_existing_setup_script(capsys):
    code = main(
        [
            "playground-command",
            "--aethyme-root",
            "packages/aethyme",
            "--source",
            "https://example.com/repo.git",
            "--name",
            "fixture",
            "--commit",
            "abc123",
            "--dest",
            "/tmp/playground",
            "--force",
        ]
    )

    assert code == 0
    output = capsys.readouterr().out
    assert "packages/aethyme/scripts/eval/setup-playground.sh" in output
    assert "--force" in output
