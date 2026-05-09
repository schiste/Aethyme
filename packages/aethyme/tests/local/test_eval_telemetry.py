"""Tests for the eval-telemetry session-mapping helper.

These tests construct synthetic Claude Code session JSONLs in tmp dirs
and verify `map_sessions_by_prompt` recovers the correct condition →
JSONL mapping. The motivating bug (chau7's `ai_session_id` reporting
the same id across multiple tabs on 2026-05-08) is the reason this
helper exists at all.
"""

from __future__ import annotations

import json
import time
from pathlib import Path

import pytest

from src.eval.telemetry import (
    DEFAULT_OUTPUT_PATH_PATTERN,
    map_sessions_by_prompt,
    missing_conditions,
    project_dir_for_repo,
)


def _write_jsonl(
    path: Path,
    *,
    output_path: str | None,
    extra_user_text: str = "",
) -> Path:
    """Write a minimal Claude Code-shaped session JSONL at `path`.

    The first user message embeds the canonical
    `.aethyme-eval-output-<cond>.json` reference so the helper can
    regex it back out. If `output_path` is None, the user message has
    no output-file reference (used to test the negative path).
    """
    user_text = "Find the bug.\n"
    if output_path:
        user_text += (
            f"You MUST save exactly one JSON object to `{output_path}` when done.\n"
        )
    user_text += extra_user_text

    lines = [
        {"type": "user", "message": {"content": user_text}, "timestamp": "2026-05-09T00:00:00Z"},
        {
            "type": "assistant",
            "message": {
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 100, "output_tokens": 10},
            },
            "timestamp": "2026-05-09T00:00:01Z",
        },
    ]
    path.write_text("\n".join(json.dumps(l) for l in lines))
    return path


# ── map_sessions_by_prompt ────────────────────────────────────────────


def test_maps_each_condition_to_its_jsonl(tmp_path: Path):
    """All 5 conditions in 5 JSONLs → 5 distinct mappings."""
    jsonls = []
    for cond in ("control-cto-off", "control-cto-on", "explore", "leverage", "task-conditioned"):
        out = f"/tmp/repo/.aethyme-eval-output-{cond}.json"
        jsonls.append(_write_jsonl(tmp_path / f"{cond}.jsonl", output_path=out))

    mapping = map_sessions_by_prompt(jsonls)
    assert set(mapping.keys()) == {
        "control-cto-off", "control-cto-on", "explore", "leverage", "task-conditioned"
    }
    for cond, path in mapping.items():
        assert path.name == f"{cond}.jsonl"


def test_picks_latest_mtime_when_multiple_match_same_condition(tmp_path: Path):
    """Re-runs leave older JSONLs for the same condition; we want the newest."""
    out = "/tmp/repo/.aethyme-eval-output-explore.json"
    older = _write_jsonl(tmp_path / "older.jsonl", output_path=out)
    newer = _write_jsonl(tmp_path / "newer.jsonl", output_path=out)
    # Make `newer` strictly newer.
    older_time = time.time() - 60.0
    older.touch()
    import os
    os.utime(older, (older_time, older_time))

    mapping = map_sessions_by_prompt([older, newer])
    assert mapping == {"explore": newer}


def test_skips_jsonls_without_user_message(tmp_path: Path):
    """A JSONL with no user message can't be condition-mapped — skip silently."""
    no_user = tmp_path / "no_user.jsonl"
    no_user.write_text(json.dumps({
        "type": "system",
        "subtype": "init",
        "message": {"content": "hi"},
    }) + "\n")
    mapping = map_sessions_by_prompt([no_user])
    assert mapping == {}


def test_skips_jsonls_whose_first_user_message_has_no_output_path(tmp_path: Path):
    """Non-eval sessions may have user messages without the output-file path."""
    chatty = _write_jsonl(tmp_path / "chatty.jsonl", output_path=None, extra_user_text="hello")
    mapping = map_sessions_by_prompt([chatty])
    assert mapping == {}


def test_skips_corrupted_jsonl_files(tmp_path: Path):
    """Truncated / non-JSON lines must not crash the mapper."""
    bad = tmp_path / "bad.jsonl"
    bad.write_text("not even json\n{\"type\": \"user\"")  # truncated
    good = _write_jsonl(
        tmp_path / "good.jsonl",
        output_path="/tmp/repo/.aethyme-eval-output-explore.json",
    )
    mapping = map_sessions_by_prompt([bad, good])
    # Bad file silently skipped; good file mapped.
    assert mapping == {"explore": good}


def test_filters_to_expected_conditions(tmp_path: Path):
    """The optional filter narrows the result to a known set."""
    for cond in ("control-cto-off", "explore", "spurious"):
        _write_jsonl(
            tmp_path / f"{cond}.jsonl",
            output_path=f"/tmp/repo/.aethyme-eval-output-{cond}.json",
        )
    mapping = map_sessions_by_prompt(
        list(tmp_path.glob("*.jsonl")),
        expected_conditions=["control-cto-off", "explore"],
    )
    assert set(mapping.keys()) == {"control-cto-off", "explore"}


def test_handles_list_content_format(tmp_path: Path):
    """Some Claude Code versions emit `content` as a list of {type, text}.

    The mapper should handle both that and the simpler string form.
    """
    list_form = tmp_path / "list_form.jsonl"
    msg = {
        "type": "user",
        "message": {
            "content": [
                {"type": "text", "text": "Save JSON to `/tmp/repo/.aethyme-eval-output-leverage.json`"}
            ],
        },
    }
    list_form.write_text(json.dumps(msg) + "\n")
    mapping = map_sessions_by_prompt([list_form])
    assert mapping == {"leverage": list_form}


# ── project_dir_for_repo ───────────────────────────────────────────────


def test_project_dir_encodes_path_with_spaces():
    """Spaces in path components become triple-dashes (Claude Code convention)."""
    encoded = project_dir_for_repo("/Users/foo/Mediawiki - Aethyme")
    assert encoded.name == "-Users-foo-Mediawiki---Aethyme"


def test_project_dir_under_dot_claude_projects():
    """The result is anchored at ~/.claude/projects/<encoded>."""
    encoded = project_dir_for_repo("/tmp/repo")
    assert ".claude" in encoded.parts
    assert "projects" in encoded.parts


# ── missing_conditions ─────────────────────────────────────────────────


def test_missing_conditions_returns_absent_keys():
    expected = ["a", "b", "c"]
    mapping = {"a": Path("/x"), "c": Path("/y")}
    assert missing_conditions(mapping, expected) == ["b"]


def test_missing_conditions_empty_when_complete():
    expected = ["a", "b"]
    mapping = {"a": Path("/x"), "b": Path("/y")}
    assert missing_conditions(mapping, expected) == []


# ── DEFAULT_OUTPUT_PATH_PATTERN ────────────────────────────────────────


def test_default_pattern_extracts_canonical_condition_names():
    """Spot-check the pattern against the 5 known conditions."""
    cases = {
        "control-cto-off": "/private/tmp/foo/.aethyme-eval-output-control-cto-off.json",
        "control-cto-on": ".aethyme-eval-output-control-cto-on.json",
        "explore": "Save to `/repo/.aethyme-eval-output-explore.json` then exit.",
        "leverage": ".aethyme-eval-output-leverage.json",
        "task-conditioned": ".aethyme-eval-output-task-conditioned.json",
    }
    for expected_cond, text in cases.items():
        m = DEFAULT_OUTPUT_PATH_PATTERN.search(text)
        assert m is not None, f"pattern failed to match: {text!r}"
        assert m.group(1) == expected_cond


def test_default_pattern_does_not_match_unrelated_text():
    assert DEFAULT_OUTPUT_PATH_PATTERN.search("no eval marker here") is None
    assert DEFAULT_OUTPUT_PATH_PATTERN.search(".aethyme-eval-result.json") is None
