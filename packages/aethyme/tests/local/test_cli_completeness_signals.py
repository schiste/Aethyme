"""CLI regression tests for completeness and confidence signal surfacing.

Migration note (python-retirement Phase 6): the in-process
monkeypatch/CliRunner tests that lived here died with the code they
tested. `test_removed_python_explore_command_prints_native_recovery_hint`
exercised `src/cli.py`'s explore tombstone — a Click `UsageError` that
pointed operators at the native binary after the 2026-05-08 hard-delete.
`src/cli.py` is gone, so `python -m src.cli explore` no longer produces a
recovery hint; it produces "No module named src". That break is
announced in README + AGENTS.md rather than shimmed (plan risk item:
"announce, don't assume"). What remains here is the surface-level test,
which drives the router subprocess.
"""

from __future__ import annotations

import json

from tests.support.cli_invoke import invoke_aethyme


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
