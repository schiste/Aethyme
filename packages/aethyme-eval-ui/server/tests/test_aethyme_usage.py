from __future__ import annotations

import importlib
import sys
from pathlib import Path


def _main_module():
    server_dir = Path(__file__).resolve().parent.parent
    if str(server_dir) not in sys.path:
        sys.path.insert(0, str(server_dir))
    return importlib.import_module("main")


def test_extract_aethyme_usage_detects_cli_commands() -> None:
    main = _main_module()

    usage = main._extract_aethyme_usage(
        [
            {
                "timestamp": "2026-04-21T10:00:00Z",
                "name": "Bash",
                "input": {
                    "command": (
                        'cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli '
                        'explore --repo "$PWD" --request "$TASK" --format answer-json'
                    )
                },
            },
            {
                "timestamp": "2026-04-21T10:00:01Z",
                "name": "Bash",
                "input": {"command": 'rg "needle" src'},
            },
        ]
    )

    assert usage["aethyme_used"] is True
    assert usage["aethyme_command_count"] == 1
    assert usage["aethyme_commands"][0]["kind"] == "explore"
    assert usage["manual_shell_after_aethyme_count"] == 1
    assert usage["manual_search_after_aethyme_count"] == 1


def test_extract_aethyme_usage_does_not_count_aethyme_repo_path_only() -> None:
    main = _main_module()

    usage = main._extract_aethyme_usage(
        [
            {
                "timestamp": "2026-04-21T10:00:00Z",
                "name": "Write",
                "input": {
                    "file_path": (
                        "/Users/me/Playground/Mediawiki/Mediawiki - Aethyme/"
                        ".aethyme-eval-output-leverage.json"
                    )
                },
            }
        ]
    )

    assert usage["aethyme_used"] is False
    assert usage["aethyme_command_count"] == 0


def test_extract_aethyme_usage_detects_eval_wrapper_command() -> None:
    main = _main_module()

    usage = main._extract_aethyme_usage(
        [
            {
                "timestamp": "2026-04-21T10:00:00Z",
                "name": "Bash",
                "input": {
                    "command": (
                        'AETHYME_TOOL=".codex/skills/aethyme/aethyme-explore"; '
                        '"$AETHYME_TOOL" --repo "$PWD" --request "$TASK" '
                        "--format answer-json"
                    )
                },
            }
        ]
    )

    assert usage["aethyme_used"] is True
    assert usage["aethyme_command_count"] == 1
    assert usage["aethyme_commands"][0]["kind"] == "explore"


def test_aethyme_usage_card_uses_wrapper_without_source_root() -> None:
    main = _main_module()

    card = main._aethyme_usage_card(
        task_text="Find behavior path",
        tool_path=".codex/skills/aethyme/aethyme-explore",
    )

    assert ".codex/skills/aethyme/aethyme-explore" in card
    assert "AETHYME_ROOT" not in card
    assert "Do not inspect benchmark/evaluator files" in card
