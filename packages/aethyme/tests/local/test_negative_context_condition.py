"""Tests for the negative-context condition wiring.

The 6th condition is plumbed across `repos.py` (CONDITION_NAMES,
TOOL_USING_CONDITIONS), `orchestrator.py` (CONDITIONS,
_EVAL_TYPE_DEFAULTS), `bug_fix.py` (prompt builder, prepare), and
the CLI (`--alternative-task` flag).

These tests pin the alignment so a future refactor that updates one
list without the other trips a deterministic failure.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from src.eval.bug_fix import (
    _LEVERAGE_NAV_CONTEXT_PATH,
    _NEGATIVE_NAV_CONTEXT_PATH,
    _build_bug_fix_prompt,
)
from src.eval.orchestrator import (
    _EVAL_TYPE_DEFAULTS,
    CONDITIONS,
    generate_run_plan,
)
from src.eval.repos import CONDITION_NAMES, TOOL_USING_CONDITIONS

# ---------------------------------------------------------------------------
# Alignment between CONDITION_NAMES (repos.py) and CONDITIONS (orchestrator)
# ---------------------------------------------------------------------------


def test_condition_names_match_orchestrator_conditions():
    """The two parallel arrays must list the same condition names in
    the same order. Repos clones one dir per CONDITION_NAMES entry;
    the orchestrator launches one tab per CONDITIONS entry. A
    mismatch silently breaks the launch phase."""
    repos_names = CONDITION_NAMES
    orch_names = tuple(c.name for c in CONDITIONS)
    assert repos_names == orch_names, (
        f"CONDITION_NAMES vs orchestrator.CONDITIONS drifted:\n"
        f"  repos.py:        {repos_names}\n"
        f"  orchestrator.py: {orch_names}"
    )


def test_negative_context_present_in_both_arrays():
    """Sanity: the new condition is wired through, not orphaned."""
    assert "negative-context" in CONDITION_NAMES
    assert "negative-context" in {c.name for c in CONDITIONS}


def test_negative_context_is_tool_using_condition():
    """The condition gets the deployed skill — agent has live tools to
    detect/correct misdirection. This is what makes the test mirror a
    stale-graph production scenario rather than a pure deception trick."""
    assert "negative-context" in TOOL_USING_CONDITIONS


# ---------------------------------------------------------------------------
# Prompt builder
# ---------------------------------------------------------------------------


def test_negative_context_prompt_points_at_negative_file():
    """The negative-context condition's prompt must reference the
    *plausibly-wrong* nav-context path. Asymmetric with leverage by
    design — leverage uses a minimal pointer (no blob path); only
    negative-context has the blob reference, because that's the
    whole point of the condition."""
    prompt = _build_bug_fix_prompt(
        Path("/tmp/repo"), "task", condition="negative-context",
    )
    assert _NEGATIVE_NAV_CONTEXT_PATH in prompt
    # The leverage blob path must NOT leak — agents shouldn't see it
    # via the negative-context prompt.
    assert _LEVERAGE_NAV_CONTEXT_PATH not in prompt


def test_leverage_prompt_uses_minimal_pointer_not_blob_path():
    """As of 2026-05-09, the leverage condition tests the depth
    ladder rather than a pre-loaded blob. The prompt must point at
    SKILL.md (and the wrapper) without naming a blob file path —
    this is the trim that revealed the negative discoverability gap
    on the prior eval. Reverting this would re-inflate leverage cost
    via cache-create overhead and is eval-tuning by another name."""
    prompt = _build_bug_fix_prompt(
        Path("/tmp/repo"), "task", condition="leverage",
    )
    assert "SKILL.md" in prompt, (
        "leverage preamble must point at SKILL.md (minimal pointer)"
    )
    assert _LEVERAGE_NAV_CONTEXT_PATH not in prompt, (
        "leverage prompt must NOT reference the nav-context blob path. "
        "If you're trying to add the blob back, see the cardinal-rule "
        "section in eval-protocol.md and `eval-tuning-rejected.md`."
    )
    assert _NEGATIVE_NAV_CONTEXT_PATH not in prompt


def test_baseline_prompt_has_no_aethyme_pointer():
    """Control / explore / task-conditioned must not name SKILL.md
    or any blob path — they're either skill-less (control) or
    discoverability-only (explore/task-conditioned)."""
    prompt = _build_bug_fix_prompt(
        Path("/tmp/repo"), "task", condition="baseline",
    )
    assert "SKILL.md" not in prompt
    assert _LEVERAGE_NAV_CONTEXT_PATH not in prompt
    assert _NEGATIVE_NAV_CONTEXT_PATH not in prompt


def test_leverage_and_negative_share_task_body():
    """Beyond their (asymmetric) preambles, the two prompts must
    share the same task body. Different bodies would mean we're
    measuring something other than 'did the wrong context mislead
    the agent'."""
    leverage = _build_bug_fix_prompt(
        Path("/tmp/repo"), "task", condition="leverage",
    )
    negative = _build_bug_fix_prompt(
        Path("/tmp/repo"), "task", condition="negative-context",
    )

    def _body_after_preamble(text: str) -> str:
        # Both prompts have preamble + blank line + body. Find the
        # first "A test is failing" line and return from there.
        lines = text.splitlines()
        for i, line in enumerate(lines):
            if line.startswith("A test is failing"):
                return "\n".join(lines[i:])
        return text

    assert _body_after_preamble(leverage) == _body_after_preamble(negative)


# ---------------------------------------------------------------------------
# _EVAL_TYPE_DEFAULTS — alternative_task is declared
# ---------------------------------------------------------------------------


def test_bug_fix_declares_alternative_task():
    """The bug-fix scenario must declare an alternative task; without
    it the negative-context condition can't generate plausibly-wrong
    context and degenerates to a leverage replay."""
    bf = _EVAL_TYPE_DEFAULTS["bug-fix"]
    alt = bf.get("alternative_task")
    assert isinstance(alt, str) and alt.strip(), (
        "_EVAL_TYPE_DEFAULTS['bug-fix']['alternative_task'] must be "
        "set to a non-empty string. See orchestrator.py."
    )
    # Must differ from the real task — otherwise it's the same context,
    # not a wrong one.
    assert alt != bf["task"]


# ---------------------------------------------------------------------------
# Run plan — negative-context surfaces in launch phase
# ---------------------------------------------------------------------------


def test_run_plan_includes_negative_context_in_launch_phase():
    """The launch phase must list the negative-context condition in
    its `conditions` array, with the same launch_method as the other
    Aethyme conditions."""
    plan = generate_run_plan(
        eval_type="bug-fix", target="grc", model="haiku",
    )
    launch = next(p for p in plan["phases"] if p["name"] == "launch")
    cond_names = [c["name"] for c in launch["conditions"]]
    assert "negative-context" in cond_names


def test_run_plan_prepare_phase_passes_alternative_task():
    """The prepare phase's cli_cmd must include `--alternative-task`
    so the negative nav-context generator runs during build-inputs."""
    plan = generate_run_plan(
        eval_type="bug-fix", target="grc", model="haiku",
    )
    prepare = next(p for p in plan["phases"] if p["name"] == "build-inputs")
    cmd = prepare.get("cli_cmd", "")
    assert "--alternative-task" in cmd, (
        "prepare phase cli_cmd is missing --alternative-task. The "
        "negative-context condition will degenerate to a leverage "
        "replay without it."
    )


def test_run_plan_paths_include_negative_context_artifacts():
    """The plan's `paths` block reserves slots for every condition's
    prompt and result file. Adding negative-context to CONDITIONS
    must not orphan its artifact paths."""
    plan = generate_run_plan(
        eval_type="bug-fix", target="grc", model="haiku",
    )
    paths = plan["paths"]
    assert "negative-context" in paths["prompt_files"]
    assert "negative-context" in paths["result_files"]
    assert "negative-context" in paths["condition_repos"]


# ---------------------------------------------------------------------------
# Scenario alignment with non-bug-fix evals (regression guard)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "eval_type",
    [
        "bug-fix-1",
        "dead-code",
        "impact-analysis",
        "feature-localization",
        "config-audit",
        "migration",
        "explain-repo",
        "navigation-ctf",
    ],
)
def test_eval_types_without_alternative_task_skip_negative_context(eval_type: str):
    """Per-eval-type opt-in: only eval types that declare
    `alternative_task` in `_EVAL_TYPE_DEFAULTS` get the 6th condition.
    The diagnostic eval types (driven by `prompts.py`) don't have a
    nav-context blob in their flow, so until we add nav-context
    plumbing there, they run with the original 5 conditions. This test
    pins the gating contract so a future change that adds
    `alternative_task` to one of these (without also adding the
    nav-context generator) trips a deterministic failure."""
    target = (
        "mediawiki" if eval_type in {
            "bug-fix-1", "dead-code", "impact-analysis",
            "feature-localization", "config-audit", "migration",
        } else "grc"
    )
    plan = generate_run_plan(
        eval_type=eval_type, target=target, model="haiku",
    )
    launch = next(p for p in plan["phases"] if p["name"] == "launch")
    cond_names = [c["name"] for c in launch["conditions"]]
    # None of these declare alternative_task today.
    assert "negative-context" not in cond_names, (
        f"{eval_type} launch phase has negative-context, but the eval "
        "type doesn't declare `alternative_task` — orchestrator "
        "filtering is broken or the eval type silently grew negative-"
        "context support without nav-context plumbing."
    )
    # And it should still have the original 5 conditions.
    for required in ("control-cto-off", "control-cto-on", "explore",
                     "leverage", "task-conditioned"):
        assert required in cond_names, (
            f"{eval_type} missing required condition {required!r}"
        )


def test_bug_fix_with_alternative_task_keeps_negative_context():
    """Inverse of the above: bug-fix DOES declare alternative_task,
    so its 6th condition runs."""
    plan = generate_run_plan(
        eval_type="bug-fix", target="grc", model="haiku",
    )
    launch = next(p for p in plan["phases"] if p["name"] == "launch")
    cond_names = [c["name"] for c in launch["conditions"]]
    assert "negative-context" in cond_names
