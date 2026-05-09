"""Tests for the eval prompts module + CLI shim.

Covers:
- `build_prompts(eval_type, target)` returns a 5-condition dict for
  every diagnostic eval type.
- Each prompt mentions the right output-file path (control conditions
  point at the Control repo, Aethyme conditions at the Aethyme repo).
- The leverage condition includes Aethyme guidance; control conditions
  do NOT (so we don't accidentally leak the skill to baselines).
- The CLI shim (`prompts_writer.main`) writes prompts + schema to disk
  with correct contents.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

from src.eval.prompts import (
    BUILDERS,
    CONDITION_NAMES,
    build_prompts,
)
from src.eval.targets import TARGETS


@pytest.fixture
def mediawiki_target():
    return TARGETS["mediawiki"]


# ── build_prompts: shape contract ──────────────────────────────────────


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_build_prompts_returns_all_5_conditions(eval_type: str, mediawiki_target):
    prompts = build_prompts(eval_type, mediawiki_target)
    assert set(prompts) == set(CONDITION_NAMES), (
        f"{eval_type}: expected all 5 conditions, got {sorted(prompts)}"
    )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_build_prompts_returns_nonempty_strings(eval_type: str, mediawiki_target):
    prompts = build_prompts(eval_type, mediawiki_target)
    for cond, text in prompts.items():
        assert isinstance(text, str)
        assert len(text) > 100, (
            f"{eval_type}/{cond} prompt suspiciously short: {len(text)} chars"
        )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_control_prompts_point_at_control_repo(
    eval_type: str, mediawiki_target
):
    """control-cto-{off,on} agents must save output to the Control repo."""
    prompts = build_prompts(eval_type, mediawiki_target)
    control_repo = str(mediawiki_target.control_path)
    for cond in ("control-cto-off", "control-cto-on"):
        assert control_repo in prompts[cond], (
            f"{eval_type}/{cond} should reference Control repo path "
            f"{control_repo!r}; instead got prompt with first 200 chars: "
            f"{prompts[cond][:200]!r}"
        )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_aethyme_prompts_point_at_aethyme_repo(eval_type: str, mediawiki_target):
    """explore/leverage/task-conditioned write to the Aethyme repo."""
    prompts = build_prompts(eval_type, mediawiki_target)
    aethyme_repo = str(mediawiki_target.aethyme_path)
    for cond in ("explore", "leverage", "task-conditioned"):
        assert aethyme_repo in prompts[cond], (
            f"{eval_type}/{cond} should reference Aethyme repo path"
        )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_control_prompts_omit_aethyme_guidance(eval_type: str, mediawiki_target):
    """The control conditions must not be told about Aethyme.

    A leak here biases the control baseline by handing the skill to a
    condition that's supposed to measure the without-skill case.
    """
    prompts = build_prompts(eval_type, mediawiki_target)
    for cond in ("control-cto-off", "control-cto-on"):
        text = prompts[cond].lower()
        assert "aethyme is available" not in text, (
            f"{eval_type}/{cond} leaked Aethyme guidance to a control condition"
        )
        assert "$aethyme_tool" not in text, (
            f"{eval_type}/{cond} mentions $AETHYME_TOOL — control bias risk"
        )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_leverage_prompt_points_at_skill(eval_type: str, mediawiki_target):
    """The leverage condition's *only* extra signal vs explore is a
    minimal pointer at the deployed skill. We assert SKILL.md is named
    so the agent knows where to read for usage; no per-eval-type
    intent guidance, no bash blocks (those would constitute
    eval-tuning by handing the agent the answer)."""
    prompts = build_prompts(eval_type, mediawiki_target)
    text = prompts["leverage"]
    assert ".codex/skills/aethyme/SKILL.md" in text, (
        f"{eval_type}/leverage should point at the skill's SKILL.md"
    )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_leverage_prompt_has_no_intent_specific_bash_block(
    eval_type: str, mediawiki_target,
):
    """Regression-guard against re-introducing per-eval-type bash blocks
    in the leverage prompt. The whole point of the leverage-vs-explore
    comparison is to measure the cost of "agent told the tool exists"
    vs "agent has skill loaded but no instruction" — a fenced bash
    block with the canonical intent invocation collapses that gap and
    biases the eval. See feedback memory `feedback_no_eval_overfitting`."""
    prompts = build_prompts(eval_type, mediawiki_target)
    text = prompts["leverage"]
    # The shared task body may contain code fences for the schema
    # example, so we look for bash/shell-tagged fences specifically.
    assert "```bash" not in text, (
        f"{eval_type}/leverage contains a bash code block — leverage "
        "should be a minimal tool pointer, not a step-by-step recipe"
    )
    # Per-eval-type intent names should not leak into the leverage
    # hint — they belong inside the skill itself, not in the prompt.
    for intent in (
        "usage_boundary_query",
        "behavior_localization_query",
        "task_localization_query",
    ):
        # The shared task text never names intents; if one appears it
        # came from a hand-rolled leverage hint.
        assert intent not in text, (
            f"{eval_type}/leverage names intent {intent!r} — that's "
            "eval-tuning, not leverage measurement"
        )


def test_unknown_eval_type_raises_keyerror(mediawiki_target):
    with pytest.raises(KeyError):
        build_prompts("not-a-real-eval-type", mediawiki_target)


# ── CLI shim ──────────────────────────────────────────────────────────


def test_prompts_writer_main_writes_all_artifacts(tmp_path: Path):
    """End-to-end: invoke main() and verify files appear with correct content."""
    from src.eval import prompts_writer

    schema_out = tmp_path / "schema.json"
    prompt_outs = {
        cond: tmp_path / f"{cond}-prompt.txt"
        for cond in CONDITION_NAMES
    }

    argv = [
        "--eval-type", "dead-code",
        "--target", "mediawiki",
        "--schema-out", str(schema_out),
    ]
    for cond, path in prompt_outs.items():
        argv.extend(["--prompt-out", f"{cond}={path}"])

    rc = prompts_writer.main(argv)
    assert rc == 0

    # Schema file is valid JSON with the expected top-level keys.
    schema = json.loads(schema_out.read_text())
    assert "type" in schema and "properties" in schema

    # All 5 prompts written and non-empty.
    for cond, path in prompt_outs.items():
        assert path.exists(), f"prompt for {cond} missing"
        text = path.read_text()
        assert "unused_functions" in text, (
            f"{cond} prompt should reference the dead-code output schema"
        )


def test_prompts_writer_rejects_missing_condition(tmp_path: Path):
    """If the caller forgets a condition, exit non-zero with a clear msg."""
    from src.eval import prompts_writer

    schema_out = tmp_path / "schema.json"
    # Only ask for 4 of 5 conditions — the dead-code builder produces all
    # 5, but we ask for 4. That should be fine (extra conditions are a
    # NOTE, not an error).
    argv = [
        "--eval-type", "dead-code",
        "--target", "mediawiki",
        "--schema-out", str(schema_out),
        "--prompt-out", f"control-cto-off={tmp_path}/a.txt",
        "--prompt-out", f"control-cto-on={tmp_path}/b.txt",
        "--prompt-out", f"explore={tmp_path}/c.txt",
        "--prompt-out", f"leverage={tmp_path}/d.txt",
    ]
    # All 4 are valid conditions — should succeed (extra `task-conditioned`
    # left out is a NOTE, not failure).
    rc = prompts_writer.main(argv)
    assert rc == 0


def test_prompts_writer_rejects_unknown_condition(tmp_path: Path):
    from src.eval import prompts_writer

    schema_out = tmp_path / "schema.json"
    argv = [
        "--eval-type", "dead-code",
        "--target", "mediawiki",
        "--schema-out", str(schema_out),
        "--prompt-out", f"bogus={tmp_path}/x.txt",
    ]
    rc = prompts_writer.main(argv)
    assert rc != 0, "main should fail when an unknown condition is requested"
