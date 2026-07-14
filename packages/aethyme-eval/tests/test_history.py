from __future__ import annotations

import json

from aethyme_eval.history import load_samples


def test_load_runs_jsonl_skips_empty_conditions_and_normalizes_targets(tmp_path):
    history = tmp_path / "runs.jsonl"
    history.write_text(
        json.dumps(
            {
                "model": "haiku",
                "target": "MediaWiki - Aethyme",
                "eval_type": "bug-fix",
                "scenario": "issue-28",
                "timestamp": "2026-05-21T12:00:00Z",
                "run_dir": "20260521T120000-mediawiki-bug-fix-haiku",
                "conditions": {
                    "control-cto-off": {
                        "total_tokens": 100,
                        "cost_usd": 0.01,
                        "duration_s": 10,
                        "quality_score": 80,
                    },
                    "explore": {
                        "total_tokens": 0,
                        "cost_usd": 0.01,
                        "duration_s": 9,
                        "quality_score": 85,
                    },
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )

    samples = load_samples(history)

    assert len(samples) == 1
    assert samples[0].key.target == "mediawiki"
    assert samples[0].key.condition == "control-cto-off"
    assert samples[0].duration_seconds == 10.0


def test_load_complete_result_reads_metadata_and_summary_metrics(tmp_path):
    run_dir = tmp_path / "20260521T121950-grc-bug-fix-haiku"
    run_dir.mkdir()
    (run_dir / "metadata.json").write_text(
        json.dumps(
            {
                "timestamp": "2026-05-21T12:19:50Z",
                "plan_run_dir": "historical-run",
            }
        ),
        encoding="utf-8",
    )
    (run_dir / "complete-result.json").write_text(
        json.dumps(
            {
                "model": {"name": "haiku"},
                "target": "/private/tmp/mockup",
                "eval_type": "bug-fix",
                "scenario": "",
                "control-cto-off": {
                    "summary_metrics": {
                        "total_tokens": 111,
                        "cost_usd": 0.02,
                        "duration_seconds": 12,
                        "quality_score": 76,
                        "global_score": 70,
                    }
                },
            }
        ),
        encoding="utf-8",
    )

    samples = load_samples(run_dir)

    assert len(samples) == 1
    sample = samples[0]
    assert sample.key.model == "haiku"
    assert sample.key.target == "grc"
    assert sample.key.scenario is None
    assert sample.run_dir == "historical-run"
    assert sample.timestamp == "2026-05-21T12:19:50Z"
    assert sample.global_score == 70.0
