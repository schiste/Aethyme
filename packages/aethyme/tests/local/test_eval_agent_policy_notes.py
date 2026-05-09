"""Tests for `agent_policy_notes` — per-condition orchestrator observations.

The field is a free-text string on each per-condition side
(`result[cond]["agent_policy_notes"]`) that the human running an
eval fills in to record what the agent actually did. It's rendered
in a `## Agent Policy Notes` section below `## Aethyme Usage` and
also persisted as a plain-text file at
`<run>/conditions/<cond>/agent-policy-notes.txt`.

The section is skipped entirely when no condition has a note set —
unannotated runs must not carry an empty header.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from src.eval.report import (
    _section_agent_policy_notes,
    write_eval_run_artifacts,
)


def _result_with_notes(notes: dict[str, str | None]) -> dict[str, Any]:
    """Build a minimal result dict where each condition has a note
    (or no `agent_policy_notes` key at all when value is None)."""
    base: dict[str, Any] = {
        "eval_type": "bug-fix-1",
        "conditions": list(notes.keys()),
    }
    for cond, note in notes.items():
        side: dict[str, Any] = {"prompt": "x", "run": {}, "assessment": {}}
        if note is not None:
            side["agent_policy_notes"] = note
        base[cond] = side
    return base


def test_section_skipped_when_no_notes_present():
    """An unannotated run must not emit an empty `## Agent Policy Notes`."""
    lines: list[str] = []
    _section_agent_policy_notes(
        lines, _result_with_notes({"explore": None, "leverage": None}),
    )
    assert lines == []


def test_section_skipped_when_notes_are_blank_strings():
    """Whitespace-only notes are treated as absent — no empty bullets."""
    lines: list[str] = []
    _section_agent_policy_notes(
        lines, _result_with_notes({"explore": "  ", "leverage": "\n\n"}),
    )
    assert lines == []


def test_section_renders_only_conditions_with_notes():
    """A run with notes on some conditions should bullet only those."""
    lines: list[str] = []
    _section_agent_policy_notes(lines, _result_with_notes({
        "control-cto-off": None,
        "explore": "agent never invoked aethyme",
        "leverage": "agent ran aethyme 3x then re-verified via grep",
    }))
    rendered = "\n".join(lines)
    assert "## Agent Policy Notes" in rendered
    assert "agent never invoked aethyme" in rendered
    assert "agent ran aethyme 3x" in rendered
    assert "control-cto-off" not in rendered.lower()


def test_section_strips_surrounding_whitespace():
    lines: list[str] = []
    _section_agent_policy_notes(lines, _result_with_notes({
        "explore": "  ran ripgrep only  \n",
    }))
    rendered = "\n".join(lines)
    assert "ran ripgrep only" in rendered
    # No double-newlines or trailing spaces sneaking in.
    assert "ran ripgrep only  " not in rendered


def test_section_uses_human_friendly_condition_labels():
    """The bullet uses `_cond_label`, so e.g. `control-cto-off` becomes
    a presentation-friendly form rather than the raw identifier. We
    don't pin the exact label here — just verify it isn't the raw
    snake-case form (which would mean the labeler wasn't called)."""
    from src.eval.report import _cond_label

    lines: list[str] = []
    _section_agent_policy_notes(lines, _result_with_notes({
        "control-cto-off": "no aethyme present",
    }))
    rendered = "\n".join(lines)
    assert _cond_label("control-cto-off") in rendered


def test_notes_persist_as_plain_text_file_per_condition(tmp_path: Path):
    """The artifact writer should drop the note as
    `conditions/<cond>/agent-policy-notes.txt` for grep/diff-friendly
    inspection. Plain text — not JSON — because the field is a human
    note, not structured data."""
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    (run_dir / "conditions" / "explore").mkdir(parents=True)
    result = _result_with_notes({"explore": "ran aethyme once and trusted it"})
    # write_eval_run_artifacts iterates active conditions; we need a
    # `run` dict for the artifact writer not to skip the condition.
    result["explore"]["run"] = {"input_tokens": 100}
    write_eval_run_artifacts(run_dir, result)
    note_path = run_dir / "conditions" / "explore" / "agent-policy-notes.txt"
    assert note_path.exists()
    assert "ran aethyme once" in note_path.read_text()


def test_no_note_file_for_conditions_without_a_note(tmp_path: Path):
    """Conditions without a note must not get an empty file (we don't
    want grep noise from runs that simply weren't annotated)."""
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    (run_dir / "conditions" / "explore").mkdir(parents=True)
    result = _result_with_notes({"explore": None})
    result["explore"]["run"] = {"input_tokens": 100}
    write_eval_run_artifacts(run_dir, result)
    assert not (
        run_dir / "conditions" / "explore" / "agent-policy-notes.txt"
    ).exists()


def test_note_round_trips_through_complete_result_json(tmp_path: Path):
    """The note must survive the full result dump — a downstream
    consumer reading `complete-result.json` should see the note on
    the condition side, not lost."""
    result = _result_with_notes({"leverage": "over-trusted the answer"})
    result["leverage"]["run"] = {"input_tokens": 100}
    dumped = json.loads(json.dumps(result))
    assert dumped["leverage"]["agent_policy_notes"] == "over-trusted the answer"
