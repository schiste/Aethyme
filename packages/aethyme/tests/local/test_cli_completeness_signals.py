"""CLI/rendering regression tests for completeness and confidence signal surfacing.

Migration note (python-retirement Phase 0): the monkeypatch-based tests
here are unit tests of the Python renderers and the Python-side explore
tombstone — implementation-specific by design, so they stay in-process
and port/retire with their code in Phase 1. Only the surface-level
intents test invokes the router subprocess.
"""

from __future__ import annotations

import json

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
