"""Tests for the eval prompts module + CLI shim.

Covers:
- `build_prompts(eval_type, target, manifest)` returns a 5-condition
  dict for every diagnostic eval type.
- Each prompt mentions the right output-file path (control conditions
  point at the Control repo, tool conditions at the tool repo).
- The leverage condition includes the tool's per-manifest guidance
  (resolved from `manifest.prompts.leverage_hint`); control conditions
  do NOT (so we don't accidentally leak the skill to baselines).
- Per-tool placeholder resolution: `{{TOOL_NAME}}` and `{{SKILL_PATH}}`
  bind to `manifest.display_name` and `manifest.prompts.skill_path` so
  graphify and aethyme produce disjoint leverage prompts.
- The CLI shim (`prompts_writer.main`) writes prompts + schema to disk
  with correct contents.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from src.eval.prompts import (
    BUILDERS,
    CONDITION_NAMES,
    build_prompts,
)
from src.eval.targets import TARGETS
from src.eval.tools.registry import get_manifest


@pytest.fixture
def mediawiki_target():
    return TARGETS["mediawiki"]


@pytest.fixture
def aethyme_manifest():
    """The in-tree Aethyme manifest — default tool binding for the eval framework.

    Tests that exercise the *default* leverage / task-conditioned hint
    text use this fixture. To verify the templating works for an
    external tool, use ``graphify_manifest`` instead.
    """
    return get_manifest("aethyme")


@pytest.fixture
def graphify_manifest():
    """A second manifest used to confirm prompts.py is tool-agnostic.

    If a test passes with ``aethyme_manifest`` but fails with this one,
    the prompt builder is leaking Aethyme-specific assumptions.
    """
    return get_manifest("graphify")


# ── build_prompts: shape contract ──────────────────────────────────────


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_build_prompts_returns_all_5_conditions(
    eval_type: str, mediawiki_target, aethyme_manifest,
):
    prompts = build_prompts(eval_type, mediawiki_target, aethyme_manifest)
    assert set(prompts) == set(CONDITION_NAMES), (
        f"{eval_type}: expected all 5 conditions, got {sorted(prompts)}"
    )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_build_prompts_returns_nonempty_strings(
    eval_type: str, mediawiki_target, aethyme_manifest,
):
    prompts = build_prompts(eval_type, mediawiki_target, aethyme_manifest)
    for cond, text in prompts.items():
        assert isinstance(text, str)
        assert len(text) > 100, (
            f"{eval_type}/{cond} prompt suspiciously short: {len(text)} chars"
        )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_control_prompts_point_at_control_repo(
    eval_type: str, mediawiki_target, aethyme_manifest,
):
    """control-cto-{off,on} agents must save output to the Control repo."""
    prompts = build_prompts(eval_type, mediawiki_target, aethyme_manifest)
    control_repo = str(mediawiki_target.control_path)
    for cond in ("control-cto-off", "control-cto-on"):
        assert control_repo in prompts[cond], (
            f"{eval_type}/{cond} should reference Control repo path "
            f"{control_repo!r}; instead got prompt with first 200 chars: "
            f"{prompts[cond][:200]!r}"
        )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_aethyme_prompts_point_at_aethyme_repo(
    eval_type: str, mediawiki_target, aethyme_manifest,
):
    """explore/leverage/task-conditioned write to the Aethyme repo."""
    prompts = build_prompts(eval_type, mediawiki_target, aethyme_manifest)
    aethyme_repo = str(mediawiki_target.aethyme_path)
    for cond in ("explore", "leverage", "task-conditioned"):
        assert aethyme_repo in prompts[cond], (
            f"{eval_type}/{cond} should reference Aethyme repo path"
        )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_control_prompts_omit_aethyme_guidance(
    eval_type: str, mediawiki_target, aethyme_manifest,
):
    """The control conditions must not be told about Aethyme.

    A leak here biases the control baseline by handing the skill to a
    condition that's supposed to measure the without-skill case.
    """
    prompts = build_prompts(eval_type, mediawiki_target, aethyme_manifest)
    for cond in ("control-cto-off", "control-cto-on"):
        text = prompts[cond].lower()
        assert "aethyme is available" not in text, (
            f"{eval_type}/{cond} leaked Aethyme guidance to a control condition"
        )
        assert "$aethyme_tool" not in text, (
            f"{eval_type}/{cond} mentions $AETHYME_TOOL — control bias risk"
        )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_leverage_prompt_points_at_skill(
    eval_type: str, mediawiki_target, aethyme_manifest,
):
    """The leverage condition's *only* extra signal vs explore is a
    minimal pointer at the deployed skill. We assert SKILL.md is named
    so the agent knows where to read for usage; no per-eval-type
    intent guidance, no bash blocks (those would constitute
    eval-tuning by handing the agent the answer)."""
    prompts = build_prompts(eval_type, mediawiki_target, aethyme_manifest)
    text = prompts["leverage"]
    assert ".codex/skills/aethyme/SKILL.md" in text, (
        f"{eval_type}/leverage should point at the skill's SKILL.md"
    )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_leverage_prompt_has_no_intent_specific_bash_block(
    eval_type: str, mediawiki_target, aethyme_manifest,
):
    """Regression-guard against re-introducing per-eval-type bash blocks
    in the leverage prompt. The whole point of the leverage-vs-explore
    comparison is to measure the cost of "agent told the tool exists"
    vs "agent has skill loaded but no instruction" — a fenced bash
    block with the canonical intent invocation collapses that gap and
    biases the eval. See feedback memory `feedback_no_eval_overfitting`."""
    prompts = build_prompts(eval_type, mediawiki_target, aethyme_manifest)
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


def test_unknown_eval_type_raises_keyerror(mediawiki_target, aethyme_manifest):
    with pytest.raises(KeyError):
        build_prompts("not-a-real-eval-type", mediawiki_target, aethyme_manifest)


# ── Manifest-driven prompt templating (Stage B.2.1) ───────────────────


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
@pytest.mark.parametrize("condition", ["leverage", "task-conditioned"])
def test_resolved_prompt_has_no_leftover_placeholders(
    eval_type: str, condition: str, mediawiki_target, aethyme_manifest,
):
    """No `{{...}}` template tokens should survive resolution.

    A surviving placeholder means either the manifest is missing a key
    or a template substitution regime forgot a placeholder. Either way
    the agent would see literal `{{TOOL_NAME}}` in its prompt — which
    biases the run and breaks reproducibility.
    """
    prompts = build_prompts(eval_type, mediawiki_target, aethyme_manifest)
    text = prompts[condition]
    assert "{{" not in text, (
        f"{eval_type}/{condition} has unresolved placeholder; first 300 "
        f"chars: {text[:300]!r}"
    )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_aethyme_manifest_resolves_tool_name_and_skill_path(
    eval_type: str, mediawiki_target, aethyme_manifest,
):
    """{{TOOL_NAME}} binds to display_name ('Aethyme'), not the slug.

    The display_name path was chosen to minimize methodology drift
    from the pre-template hardcoded string 'Aethyme is available...'.
    Binding to the lowercase slug instead would silently re-cap the
    prompts and change every methodology_hash.
    """
    prompts = build_prompts(eval_type, mediawiki_target, aethyme_manifest)
    leverage = prompts["leverage"]
    assert "Aethyme" in leverage, (
        f"{eval_type}/leverage should mention 'Aethyme' (display_name); "
        f"first 300 chars: {leverage[:300]!r}"
    )
    assert ".codex/skills/aethyme/SKILL.md" in leverage, (
        f"{eval_type}/leverage should reference skill_path"
    )


@pytest.mark.parametrize("eval_type", sorted(BUILDERS))
def test_graphify_manifest_resolves_tool_name_and_skill_path(
    eval_type: str, mediawiki_target, graphify_manifest,
):
    """Same regression check as the aethyme variant, but with a foreign
    tool. If this passes, prompts.py is genuinely tool-agnostic; if
    only the aethyme test passes, the builder is still leaking
    Aethyme-specific assumptions somewhere."""
    prompts = build_prompts(eval_type, mediawiki_target, graphify_manifest)
    leverage = prompts["leverage"]
    assert "Graphify" in leverage, (
        f"{eval_type}/leverage should mention 'Graphify' (display_name); "
        f"first 300 chars: {leverage[:300]!r}"
    )
    assert "CLAUDE.md" in leverage, (
        f"{eval_type}/leverage should reference graphify's skill_path"
    )
    # The aethyme-specific skill path must not leak when a different
    # manifest is in play.
    assert ".codex/skills/aethyme" not in leverage, (
        f"{eval_type}/leverage leaked the aethyme skill path while "
        f"rendering graphify's manifest"
    )


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
