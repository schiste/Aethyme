"""Tests for the per-eval-type comparison contract.

Each eval-type entry in `_EVAL_TYPE_DEFAULTS` carries an `objective`
(what we're using the eval to compare across conditions) and a
`constraints` tuple (gates the answer must satisfy to count). These
travel into the run plan's meta dict and are rendered into the report
header so a reader doesn't have to reverse-engineer what the table
measures. They are NEVER injected into agent prompts — that would be
eval-tuning by handing the agent the rubric.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from src.eval.orchestrator import (
    _EVAL_TYPE_DEFAULTS,
    generate_run_plan,
    get_eval_type_contract,
)
from src.eval.report import _render_objective_and_constraints


@pytest.mark.parametrize("eval_type", sorted(_EVAL_TYPE_DEFAULTS))
def test_every_eval_type_declares_objective_and_constraints(eval_type: str):
    """Every eval-type defaults entry must declare a non-empty
    objective and at least one constraint. New eval types added in
    the future inherit this requirement — the contract is what makes
    the report self-explanatory."""
    objective, constraints = get_eval_type_contract(eval_type)
    assert objective, f"{eval_type} missing objective"
    assert constraints, f"{eval_type} missing constraints"
    # Sanity: the objective should reference comparison or efficiency
    # (the whole point) rather than reading like a task restatement.
    assert "compare" in objective.lower() or "comparison" in objective.lower(), (
        f"{eval_type} objective should mention comparison/efficiency: "
        f"{objective!r}"
    )


def test_unknown_eval_type_returns_empty_contract():
    """Legacy or unknown eval types must not crash — the report
    layer falls back to omitting the section."""
    objective, constraints = get_eval_type_contract("not-a-real-eval-type")
    assert objective == ""
    assert constraints == []


@pytest.mark.parametrize("eval_type", sorted(_EVAL_TYPE_DEFAULTS))
def test_run_plan_meta_carries_contract(eval_type: str):
    """The plan's meta dict must include objective + constraints, so
    consumers reading the plan (or the result derived from it) have
    them available without re-deriving."""
    target = (
        "mediawiki" if _EVAL_TYPE_DEFAULTS[eval_type].get("target_restriction") == "mediawiki"
        else "grc"
    )
    plan = generate_run_plan(eval_type=eval_type, target=target, model="haiku")
    meta = plan["meta"]
    assert meta["objective"], f"{eval_type} plan meta missing objective"
    assert meta["constraints"], f"{eval_type} plan meta missing constraints"
    # constraints must round-trip as a list (JSON-serializable for the
    # plan dump), not a tuple.
    assert isinstance(meta["constraints"], list)


def test_report_renders_contract_for_known_eval_type():
    """Smoke test: feeding a result dict with a known eval_type to
    the contract renderer produces both Objective and Constraints
    sections in the expected order."""
    lines: list[str] = []
    result: dict[str, Any] = {"eval_type": "bug-fix-1"}
    _render_objective_and_constraints(lines, result)
    rendered = "\n".join(lines)
    assert "## Objective" in rendered
    assert "## Constraints" in rendered
    # Objective comes before constraints in the header.
    assert rendered.index("## Objective") < rendered.index("## Constraints")


def test_report_falls_back_to_explicit_result_fields():
    """If a result dict already carries `objective`/`constraints`
    (e.g. a legacy result, or one captured at eval time before the
    defaults changed), prefer those over the defaults lookup. The
    report should reflect what the eval was actually measured against."""
    lines: list[str] = []
    result: dict[str, Any] = {
        "eval_type": "bug-fix-1",
        "objective": "CUSTOM OBJECTIVE",
        "constraints": ["custom constraint A", "custom constraint B"],
    }
    _render_objective_and_constraints(lines, result)
    rendered = "\n".join(lines)
    assert "CUSTOM OBJECTIVE" in rendered
    assert "custom constraint A" in rendered
    assert "custom constraint B" in rendered
    # Defaults lookup should NOT have been used.
    default_obj, _ = get_eval_type_contract("bug-fix-1")
    assert default_obj not in rendered


def test_report_no_op_for_unknown_eval_type():
    """Result dicts without objective/constraints AND with an
    unknown eval_type must not raise; the report just skips the
    section."""
    lines: list[str] = []
    result: dict[str, Any] = {"eval_type": "definitely-not-real"}
    _render_objective_and_constraints(lines, result)
    assert lines == []


def test_objective_and_constraints_not_in_agent_prompts():
    """Cardinal rule: the contract is for the human reader, not the
    agent. Confirm none of the per-condition prompts contain the
    objective or constraint strings — that would be eval-tuning by
    handing the agent the rubric."""
    from src.eval.prompts import build_prompts
    from src.eval.targets import TARGETS
    from src.eval.tools.registry import get_manifest

    target = TARGETS["mediawiki"]
    manifest = get_manifest("aethyme")
    objective, constraints = get_eval_type_contract("bug-fix-1")
    # Take a salient phrase from each that is unlikely to appear by
    # accident in the task text.
    objective_phrase = "comparison axis"
    prompts = build_prompts("bug-fix-1", target, manifest)
    for cond, text in prompts.items():
        assert objective_phrase not in text, (
            f"bug-fix-1/{cond} prompt leaks the objective phrase "
            f"{objective_phrase!r} — that's eval-tuning"
        )
        for c in constraints:
            # The 'must be saved to the agent-specified path' wording
            # in constraints is shaped to be specific (no agent will
            # phrase its own task text exactly that way), so a
            # substring leak would be diagnostic.
            distinctive = "agent-specified path"
            if distinctive in c:
                assert distinctive not in text, (
                    f"bug-fix-1/{cond} prompt leaks constraint phrase "
                    f"{distinctive!r}"
                )


# Defensive: catch the trap where someone "improves" the orchestrator
# by inserting objective into prompts as a guidance string.
def test_prompts_module_does_not_import_orchestrator():
    """Module-level guard: `prompts.py` must not import from
    orchestrator. The two modules stay decoupled so a future
    contributor can't accidentally plumb objective strings into
    agent prompts. Uses AST so docstring mentions of 'orchestrator'
    don't false-positive."""
    import ast

    prompts_path = (
        Path(__file__).resolve().parents[2]
        / "src" / "eval" / "prompts.py"
    )
    tree = ast.parse(prompts_path.read_text())
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom):
            module = node.module or ""
            assert "orchestrator" not in module, (
                f"prompts.py imports from {module!r} — keeps the "
                "comparison contract out of agent prompts. See "
                "feedback_no_eval_overfitting."
            )
        elif isinstance(node, ast.Import):
            for alias in node.names:
                assert "orchestrator" not in alias.name, (
                    f"prompts.py imports {alias.name!r} — keeps the "
                    "comparison contract out of agent prompts."
                )
        elif isinstance(node, ast.Name) and node.id == "_EVAL_TYPE_DEFAULTS":
            pytest.fail(
                "prompts.py references _EVAL_TYPE_DEFAULTS — must not "
                "reach into the eval-type defaults table"
            )
