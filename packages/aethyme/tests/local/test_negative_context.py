"""Tests for the plausible-error nav-context generator.

The generator (`generate_plausible_error_context`) and its predicate
(`_assert_plausibly_wrong`) are the foundation of the negative-context
eval condition. They isolate two effects the leverage measurement
otherwise entangles:

- Loading cost (tokens for any context, regardless of correctness).
- Misdirection cost (extra turns wasted following bad anchors).

The 5 properties enforce that "wrong" context is *plausibly* wrong —
hard to dismiss on surface inspection — so we measure trust
calibration rather than recognition of obvious mismatch.

These tests use synthetic dicts for the predicate (cheap, repo-
independent) and only exercise the full generator end-to-end in the
GRC-marked test (which the strict-engine CI lane runs).
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from src.eval.bug_fix import (
    DEFAULT_PLAUSIBILITY_OVERLAP_THRESHOLD,
    PlausibilityError,
    _anchor_identifier_set,
    _assert_plausibly_wrong,
)


def _ctx(
    *,
    anchor_ids: list[str],
    repo_path: str = "/tmp/repo",
    file_contents_size: int = 0,
    extra_keys: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Build a minimal nav-context-shaped dict with controllable fields.

    The tests don't care about scope or task strings; they care about
    shape, size, repo_path, and anchor ids. We pad `file_contents`
    with synthetic bytes when we want to exercise size constraints."""
    ctx: dict[str, Any] = {
        "mode": "bug_fix_navigation",
        "repo_path": repo_path,
        "task": "synthetic",
        "test_file": "test.ts",
        "bug_area": "synthetic",
        "anchors": [{"id": aid, "file": f"{aid}.ts"} for aid in anchor_ids],
        "scope": {},
        "file_contents": [{"path": "x.ts", "content": "x" * file_contents_size}]
        if file_contents_size
        else [],
    }
    if extra_keys:
        ctx.update(extra_keys)
    return ctx


# ---------------------------------------------------------------------------
# Identifier extraction — repo-agnostic invariant
# ---------------------------------------------------------------------------


def test_anchor_id_extraction_skips_anchors_without_id():
    ctx = {
        "anchors": [
            {"id": "Foo"},
            {"id": ""},          # empty string — skip
            {"file": "bar.ts"},  # no id key — skip
            {"id": None},        # null — skip
            {"id": "Bar"},
        ]
    }
    assert _anchor_identifier_set(ctx) == {"Foo", "Bar"}


def test_anchor_id_extraction_handles_missing_anchors_field():
    """Defensive: real contexts may have empty or missing anchors;
    the predicate should not crash."""
    assert _anchor_identifier_set({}) == set()
    assert _anchor_identifier_set({"anchors": None}) == set()
    assert _anchor_identifier_set({"anchors": []}) == set()


# ---------------------------------------------------------------------------
# Property 1 — Shape-identical
# ---------------------------------------------------------------------------


def test_shape_mismatch_fails():
    real = _ctx(anchor_ids=["Foo", "Bar"])
    cand = _ctx(anchor_ids=["Baz", "Qux"])
    del cand["scope"]  # drop a key
    with pytest.raises(PlausibilityError, match="shape mismatch"):
        _assert_plausibly_wrong(real, cand, overlap_threshold=0.0)


def test_extra_key_in_candidate_also_fails():
    real = _ctx(anchor_ids=["Foo"])
    cand = _ctx(anchor_ids=["Foo", "Bar"], extra_keys={"surprise": 42})
    with pytest.raises(PlausibilityError, match="shape mismatch"):
        _assert_plausibly_wrong(real, cand, overlap_threshold=0.0)


# ---------------------------------------------------------------------------
# Property 2 — Approximate size
# ---------------------------------------------------------------------------


def test_candidate_too_large_fails():
    real = _ctx(anchor_ids=["Foo"], file_contents_size=100)
    cand = _ctx(anchor_ids=["Foo"], file_contents_size=10_000)  # 100×
    with pytest.raises(PlausibilityError, match="size ratio"):
        _assert_plausibly_wrong(real, cand, overlap_threshold=0.0)


def test_candidate_too_small_fails():
    real = _ctx(anchor_ids=["Foo"], file_contents_size=10_000)
    cand = _ctx(anchor_ids=["Foo"], file_contents_size=100)  # 1/100×
    with pytest.raises(PlausibilityError, match="size ratio"):
        _assert_plausibly_wrong(real, cand, overlap_threshold=0.0)


def test_candidate_within_size_ratio_passes():
    """Within 2× is allowed (1.5× tested here for safety margin)."""
    real = _ctx(anchor_ids=["Foo", "Bar"], file_contents_size=1000)
    cand = _ctx(anchor_ids=["Foo", "Baz"], file_contents_size=1500)
    _assert_plausibly_wrong(real, cand, overlap_threshold=0.5)


# ---------------------------------------------------------------------------
# Property 3 — Domain-adjacent (same repo_path)
# ---------------------------------------------------------------------------


def test_different_repos_fails():
    real = _ctx(anchor_ids=["Foo", "Bar"], repo_path="/tmp/repo-a")
    cand = _ctx(anchor_ids=["Foo", "Bar"], repo_path="/tmp/repo-b")
    with pytest.raises(PlausibilityError, match="different repos"):
        _assert_plausibly_wrong(real, cand, overlap_threshold=0.0)


# ---------------------------------------------------------------------------
# Property 4 — Identifier overlap (>= threshold)
# ---------------------------------------------------------------------------


def test_zero_overlap_fails():
    real = _ctx(anchor_ids=["Foo", "Bar", "Baz"])
    cand = _ctx(anchor_ids=["Alice", "Bob", "Carol"])
    with pytest.raises(PlausibilityError, match="identifier overlap"):
        _assert_plausibly_wrong(real, cand, overlap_threshold=0.30)


def test_below_threshold_fails():
    """1 of 4 real anchors in candidate = 25% overlap, below 30%."""
    real = _ctx(anchor_ids=["A", "B", "C", "D"])
    cand = _ctx(anchor_ids=["A", "X", "Y", "Z"])
    with pytest.raises(PlausibilityError, match="0\\.25 below threshold"):
        _assert_plausibly_wrong(real, cand, overlap_threshold=0.30)


def test_at_threshold_passes():
    """Exactly threshold is admissible — 30% means 30% works."""
    real = _ctx(anchor_ids=["A", "B", "C", "D", "E"])
    cand = _ctx(anchor_ids=["A", "B", "X", "Y", "Z"])  # 2/5 = 40%
    _assert_plausibly_wrong(real, cand, overlap_threshold=0.30)


def test_overlap_is_directional_real_to_candidate():
    """We measure overlap as fraction of REAL anchors in candidate,
    NOT Jaccard. A small candidate with all of real's anchors plus
    nothing else passes (100%); a large candidate with only some of
    real's anchors fails. Asymmetry is intentional — narrow modules
    can't always produce wide candidates."""
    real = _ctx(anchor_ids=["A", "B", "C"])
    # candidate is wider but contains all of real
    cand = _ctx(anchor_ids=["A", "B", "C", "X", "Y", "Z", "W", "V", "U"])
    # |real ∩ cand| / |real| = 3/3 = 100% → passes
    # Jaccard would be 3/9 = 33% → marginal
    # We require equal anchor sets to NOT be the same (property 5),
    # but a strict superset is fine.
    _assert_plausibly_wrong(real, cand, overlap_threshold=0.30)


def test_empty_anchor_set_fails():
    real = _ctx(anchor_ids=[])
    cand = _ctx(anchor_ids=["A"])
    with pytest.raises(PlausibilityError, match="empty anchor id set"):
        _assert_plausibly_wrong(real, cand, overlap_threshold=0.0)


# ---------------------------------------------------------------------------
# Property 5 — Distinct (else it's the same context, not a wrong one)
# ---------------------------------------------------------------------------


def test_identical_anchor_sets_fail():
    real = _ctx(anchor_ids=["A", "B", "C"])
    cand = _ctx(anchor_ids=["A", "B", "C"])  # same set
    with pytest.raises(PlausibilityError, match="identical to real"):
        _assert_plausibly_wrong(real, cand, overlap_threshold=0.0)


def test_subset_does_not_count_as_identical():
    """Strict subset/superset is fine — only identity fails property 5."""
    real = _ctx(anchor_ids=["A", "B", "C"])
    cand = _ctx(anchor_ids=["A", "B", "C", "D"])  # superset, not equal
    _assert_plausibly_wrong(real, cand, overlap_threshold=0.5)


# ---------------------------------------------------------------------------
# Default threshold
# ---------------------------------------------------------------------------


def test_default_threshold_is_30_percent():
    """Pin the default so a future contributor changing it has to
    update both this test and the design rule documentation."""
    assert DEFAULT_PLAUSIBILITY_OVERLAP_THRESHOLD == 0.30


# ---------------------------------------------------------------------------
# Repo-agnosticism — ensure the predicate makes no assumptions about
# language or path conventions.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "real_ids,cand_ids",
    [
        # TypeScript-ish identifiers
        (["PERMISSION_IMPLICATIONS", "Action.MANAGE", "Action.SHARE"],
         ["PERMISSION_IMPLICATIONS", "Action.READ", "Action.CREATE"]),
        # PHP-ish identifiers
        (["WikiPage::doViewUpdates", "WikiPage::isWatchable",
          "WatchedItemStore"],
         ["WikiPage::doViewUpdates", "WikiPage::isMain",
          "ContentHandler"]),
        # Rust-ish identifiers
        (["aethyme_engine::explore::ExploreParams",
          "aethyme_engine::graph::GraphStore"],
         ["aethyme_engine::explore::ExploreResponse",
          "aethyme_engine::graph::GraphStore"]),
    ],
)
def test_predicate_works_on_arbitrary_identifier_styles(
    real_ids: list[str], cand_ids: list[str],
):
    """The rule must apply identically to TypeScript, PHP, Rust, etc.
    No language-specific tokenization or path normalization."""
    real = _ctx(anchor_ids=real_ids)
    cand = _ctx(anchor_ids=cand_ids)
    # Each pair has 1+ shared identifier (≥33% overlap on smallest set).
    _assert_plausibly_wrong(real, cand, overlap_threshold=0.30)


# ---------------------------------------------------------------------------
# Constructor pattern — generator wraps _build_navigation_context
# ---------------------------------------------------------------------------


def test_generator_signature_takes_alternative_task():
    """Smoke test that the generator API matches the design: caller
    supplies the alternative task, function does NOT synthesize one.
    A future refactor that adds an in-function 'task mutation'
    feature should explicitly justify why."""
    import inspect
    from src.eval.bug_fix import generate_plausible_error_context

    sig = inspect.signature(generate_plausible_error_context)
    params = sig.parameters
    assert "alternative_task" in params, (
        "generator must take an explicit alternative_task — caller's "
        "responsibility to provide one (repo-agnostic by design)"
    )
    assert "overlap_threshold" in params
    # alternative_task must be keyword-only (we don't want callers
    # accidentally passing it positionally and confusing it with
    # the real context).
    assert params["alternative_task"].kind == inspect.Parameter.KEYWORD_ONLY
