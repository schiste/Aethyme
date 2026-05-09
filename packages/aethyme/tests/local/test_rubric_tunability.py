"""Tests for per-component `tunability` annotations on scoring rubrics.

`tunability` describes how susceptible a rubric component is to
deliberate gaming (e.g., return more candidates to inflate recall;
sprinkle keywords to inflate prose-quality). It's an annotation
shown to the human reviewer, NOT a scoring input.

Cardinal-rule guard: weights must NOT change in the same commit
that adds tunability annotations. Reviewers reading a low-effort
"rebalance based on tunability" PR should reject it without an
independent measurement-bias study; this test fences off the
trivial case where someone "improves" weights based purely on
their own tunability assessment.
"""

from __future__ import annotations

import pytest

from src.eval.schemas import (
    bug_fix_scoring_rubric,
    explain_repo_scoring_rubric,
    mediawiki_bug_fix_1_scoring_rubric,
    mediawiki_dead_code_scoring_rubric,
    mediawiki_migration_scoring_rubric,
    navigation_ctf_scoring_rubric,
    onboarding_auth_scoring_rubric,
)

_ALL_RUBRICS = {
    "bug-fix": bug_fix_scoring_rubric,
    "bug-fix-1": mediawiki_bug_fix_1_scoring_rubric,
    "explain-repo": explain_repo_scoring_rubric,
    "navigation-ctf": navigation_ctf_scoring_rubric,
    "onboarding-auth": onboarding_auth_scoring_rubric,
    "dead-code": mediawiki_dead_code_scoring_rubric,
    "migration": mediawiki_migration_scoring_rubric,
}

_VALID_TUNABILITY_VALUES = {"low", "medium", "high"}

# Pinned weight totals — *NOT* the values, just the sum-to-100
# invariant. The values themselves can change in PRs that justify
# the change with measurement evidence; the sum check ensures any
# accidental rebalance from the tunability annotations will trip
# this test.
_EXPECTED_WEIGHT_SUMS: dict[str, int] = {
    "bug-fix": 100,
    "bug-fix-1": 100,
    "explain-repo": 100,
    "navigation-ctf": 100,
    "onboarding-auth": 100,
    "dead-code": 100,
    "migration": 100,
}


@pytest.mark.parametrize("name,builder", sorted(_ALL_RUBRICS.items()))
def test_every_rubric_carries_tunability_dict(
    name: str, builder,
):
    """Every rubric must declare a tunability dict alongside weights."""
    rubric = builder()
    assert "tunability" in rubric, (
        f"rubric {name!r} missing `tunability` annotations"
    )
    assert isinstance(rubric["tunability"], dict)
    assert rubric["tunability"], (
        f"rubric {name!r} tunability dict is empty"
    )


@pytest.mark.parametrize("name,builder", sorted(_ALL_RUBRICS.items()))
def test_tunability_keys_match_weights_keys(
    name: str, builder,
):
    """The two dicts must cover the same components — a missing
    tunability value (or an extra one) is a maintenance bug."""
    rubric = builder()
    weights_keys = set(rubric["weights"].keys())
    tunability_keys = set(rubric["tunability"].keys())
    assert weights_keys == tunability_keys, (
        f"rubric {name!r}: weights keys {sorted(weights_keys)} "
        f"!= tunability keys {sorted(tunability_keys)}"
    )


@pytest.mark.parametrize("name,builder", sorted(_ALL_RUBRICS.items()))
def test_tunability_values_are_valid_levels(
    name: str, builder,
):
    """Only `low`, `medium`, `high` are accepted. Free-form values
    would dilute the legend and make cross-rubric comparison
    meaningless."""
    rubric = builder()
    for component, level in rubric["tunability"].items():
        assert level in _VALID_TUNABILITY_VALUES, (
            f"rubric {name!r}/{component}: invalid tunability "
            f"value {level!r} (must be one of "
            f"{sorted(_VALID_TUNABILITY_VALUES)})"
        )


@pytest.mark.parametrize("name,builder", sorted(_ALL_RUBRICS.items()))
def test_weights_still_sum_to_expected_total(
    name: str, builder,
):
    """Cardinal-rule guard: tunability annotations must NOT have
    altered the weight totals. If this test fails, the change
    making it fail is a weight rebalance disguised as an
    annotation — that's the eval-tuning trap. Either revert the
    weight change or split it into a separate PR with explicit
    measurement-bias evidence."""
    rubric = builder()
    actual = sum(rubric["weights"].values())
    expected = _EXPECTED_WEIGHT_SUMS[name]
    assert actual == expected, (
        f"rubric {name!r} weight sum changed: {actual} (was {expected}). "
        "If this is a legitimate rebalance, update the expected total. "
        "If it leaked in alongside tunability annotations, revert."
    )


@pytest.mark.parametrize("name,builder", sorted(_ALL_RUBRICS.items()))
def test_legend_appears_in_notes(name: str, builder):
    """The reader-facing legend must appear in every rubric's notes,
    so a downstream consumer rendering the rubric (e.g., the report
    layer) gets the explanation alongside the values rather than
    having to look up the constant."""
    rubric = builder()
    notes = rubric.get("notes", [])
    legend_present = any(
        isinstance(n, str) and "tunability" in n.lower() and "low =" in n
        for n in notes
    )
    assert legend_present, (
        f"rubric {name!r}: tunability legend missing from notes"
    )


def test_dead_code_precision_marked_high_tunability():
    """Sanity-check that the most pedagogically clear case is
    annotated correctly. The dead-code precision component
    rewards listing fewer candidates; that's a textbook
    high-tunability situation. If a future contributor reweights
    this to 'low', they need to add evidence — and this test
    ensures they at least notice they're doing it."""
    rubric = mediawiki_dead_code_scoring_rubric()
    assert rubric["tunability"]["false_positives"] == "high"


def test_navigation_ctf_targets_marked_low_tunability():
    """Exact-path matches are the canonical low-tunability case
    (you either return the path or you don't)."""
    rubric = navigation_ctf_scoring_rubric()
    assert rubric["tunability"]["config_target"] == "low"
    assert rubric["tunability"]["code_target"] == "low"
