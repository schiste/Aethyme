"""Run-dir naming contract.

The eval-runs/ directory is the long-term record of every eval that's ever
been run on this repo. Stable, greppable names matter:

    eval-runs/{timestamp}-{target}-[{scenario}-]{eval-type}-{model}[-{reasoning}][-{tool}]

Each segment beyond timestamp+target+eval-type is included only when it adds
discrimination value. Tests below pin the contract so a refactor of
``generate_run_plan``'s slug-building logic surfaces any unintended drift
in dir layout — affecting reports, archived analyses, and any future
tooling that grep-discovers prior runs.
"""

from __future__ import annotations

import re

import pytest

from src.eval.orchestrator import generate_run_plan


def _run_dir(**kwargs: object) -> str:
    """Extract just the run-dir basename (timestamp stripped) for assertions.

    Tests below assert against the slug part — the timestamp prefix is
    non-deterministic across runs and not part of the contract under test.
    """
    plan = generate_run_plan(**kwargs)  # type: ignore[arg-type]
    full = plan["paths"]["run_dir"]
    assert full.startswith("eval-runs/"), full
    name = full[len("eval-runs/"):]
    # Strip leading "YYYYMMDDTHHMMSS-"
    match = re.match(r"^\d{8}T\d{6}-(.+)$", name)
    assert match, f"run_dir name does not start with timestamp: {name!r}"
    return match.group(1)


def test_default_run_dir_includes_target_evaltype_model():
    """Minimal run with all defaults gets target + eval-type + model."""
    assert _run_dir(eval_type="dead-code", target="mediawiki", model="haiku") == (
        "mediawiki-dead-code-haiku"
    )


def test_reasoning_appended_when_non_default():
    """Reasoning level is appended after model when set explicitly."""
    assert _run_dir(eval_type="dead-code", target="mediawiki", model="haiku",
                    reasoning="low") == "mediawiki-dead-code-haiku-low"
    assert _run_dir(eval_type="dead-code", target="mediawiki", model="haiku",
                    reasoning="high") == "mediawiki-dead-code-haiku-high"


def test_reasoning_default_is_omitted():
    """``reasoning='default'`` must not appear in the dir name (it's the default)."""
    assert _run_dir(eval_type="dead-code", target="mediawiki", model="haiku",
                    reasoning="default") == "mediawiki-dead-code-haiku"


def test_tool_omitted_when_aethyme():
    """Default tool=aethyme is omitted to preserve historical naming compatibility."""
    assert _run_dir(eval_type="bug-fix", target="grc", model="haiku",
                    tool="aethyme") == "grc-bug-fix-haiku"


def test_tool_appended_when_competitor():
    """Non-default tools are explicit in the dir name for cross-tool grep-ability."""
    assert _run_dir(eval_type="bug-fix", target="grc", model="haiku",
                    tool="graphify") == "grc-bug-fix-haiku-graphify"


def test_scenario_inserted_between_target_and_evaltype():
    """``cross-package`` scenario preserves prior placement (between target and eval-type)."""
    assert _run_dir(eval_type="bug-fix", target="grc", model="haiku",
                    scenario="cross-package") == "grc-cross-package-bug-fix-haiku"


def test_all_segments_compose_in_documented_order():
    """The full-matrix shape: target-scenario-evaltype-model-reasoning-tool."""
    assert _run_dir(
        eval_type="bug-fix", target="grc", model="haiku",
        scenario="cross-package", reasoning="high", tool="graphify",
    ) == "grc-cross-package-bug-fix-haiku-high-graphify"


@pytest.mark.parametrize("eval_type,target", [
    ("bug-fix", "grc"),
    ("bug-fix-1", "mediawiki"),
    ("dead-code", "mediawiki"),
    ("explain-repo", "grc"),
    ("navigation-ctf", "grc"),
    ("impact-analysis", "mediawiki"),
    ("feature-localization", "mediawiki"),
    ("config-audit", "mediawiki"),
    ("migration", "mediawiki"),
])
def test_run_dir_is_always_unique_with_seconds_timestamp(eval_type: str, target: str):
    """No matter the eval-type, the run-dir contains a second-precision
    timestamp prefix that prevents same-day collisions.

    This is the core "uniqueness" contract — a user running two evals
    of the same eval-type+target+model on the same day still gets
    distinct dirs.
    """
    plan = generate_run_plan(eval_type=eval_type, target=target, model="haiku")
    full = plan["paths"]["run_dir"]
    # YYYYMMDDTHHMMSS must be present right after eval-runs/
    assert re.match(r"^eval-runs/\d{8}T\d{6}-", full), full
