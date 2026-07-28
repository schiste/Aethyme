"""CLI/rendering regression tests for completeness and confidence signal surfacing.

Migration note (python-retirement Phase 0): the monkeypatch-based tests
here are unit tests of the Python renderers and the Python-side explore
tombstone — implementation-specific by design, so they stay in-process
and port/retire with their code in Phase 1. Only the surface-level
intents test invokes the router subprocess.
"""

from __future__ import annotations

import json
from pathlib import Path

from click.testing import CliRunner

from src.cli import cli
from tests.support.cli_invoke import invoke_aethyme


def test_removed_python_explore_command_prints_native_recovery_hint() -> None:
    runner = CliRunner()
    result = runner.invoke(cli, ["explore", "--repo", "/tmp/repo", "--request", "task"])

    assert result.exit_code == 2
    assert "'explore' was removed from the Python CLI on 2026-05-08" in result.output
    assert '"$AETHYME_ROOT/rust/target/release/aethyme" explore' in result.output
    assert "The Python CLI still handles graph, task, intents, facts, and analyze." in result.output


def test_analyze_dead_code_eval_json_is_task_ready(
    monkeypatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo"
    repo_path.mkdir(parents=True)

    monkeypatch.setattr(
        "src.cli.analyze_dead_code_answer",
        lambda _repo, _scope, roots, include_methods: {
            "analyzer": "dead-code",
            "version": "2",
            "query": {
                "scope": "src/indexing",
                "searched_roots": roots,
                "include_methods": include_methods,
            },
            "candidates": [
                {
                    "function": {
                        "name": "unused_helper",
                        "defined_in": "src/indexing/service.py",
                    },
                    "status": "Unused",
                    "confidence": 0.95,
                    "evidence": {
                        "searched_roots": roots,
                        "external_callers": [],
                        "internal_callers": [],
                        "docs_config_references": ["doc:docs/service.md"],
                    },
                    "ambiguity": [],
                    "rationale": "No external callers found under [src].",
                }
            ],
            "excluded": [],
            "summary": {"total_candidates": 1, "unused": 1, "ambiguous": 0, "used": 0},
            "observability": {
                "graph_counts": {"functions": 1, "docs": 1, "configs": 0, "edges": 2},
                "fact_counts": {
                    "public_functions": 1,
                    "usage_facts": 1,
                    "internal_callers": 0,
                    "external_callers": 0,
                    "docs_config_references": 1,
                },
                "confidence_summary": {
                    "high": 1,
                    "medium": 0,
                    "low": 0,
                    "min": 0.95,
                    "max": 0.95,
                },
                "degraded_reasons": [],
            },
        },
    )

    runner = CliRunner()
    result = runner.invoke(
        cli,
        [
            "analyze",
            "dead-code",
            "--repo",
            str(repo_path),
            "--scope",
            "src/indexing",
            "--roots",
            "src,tests",
            "--format",
            "eval-json",
            "--show-observability",
        ],
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["unused_functions"] == [
        {
            "function_name": "unused_helper",
            "defined_in": "src/indexing/service.py",
            "status": "Unused",
            "external_callers": [],
            "internal_callers": [],
            "evidence": {
                "searched_roots": ["src", "tests"],
                "external_callers": [],
                "internal_callers": [],
                "docs_config_references": ["doc:docs/service.md"],
                "ambiguity": [],
            },
            "confidence": 0.95,
            "reason": "No external callers found under [src].",
        }
    ]
    assert payload["excluded_functions"] == []
    assert payload["observability"]["command"] == "analyze dead-code"
    assert (
        payload["observability"]["index_freshness"]["status"]
        == "fresh_for_current_snapshot"
    )
    assert (
        payload["observability"]["graph_fact_count"]["facts"]["public_functions"] == 1
    )


def test_intents_compact_json_lists_default_task_localization_query() -> None:
    result = invoke_aethyme(["intents", "--format", "compact-json"])

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    explore_mode = next(
        mode for mode in payload["modes"] if mode["mode"] == "explore"
    )
    intent = explore_mode["intents"][0]
    assert intent["intent"] == "task_localization_query"
    assert intent["required_params"] == []
    assert intent["default_for_explore"] is True
    assert "answer_schema" in intent
    assert "observability" in intent
    intent_names = {item["intent"] for item in explore_mode["intents"]}
    assert "behavior_localization_query" in intent_names
    assert "usage_boundary_query" in intent_names
