"""Unit tests for src/eval/probes.

Covers:
  - Navigation probe: hits, misses, distractors, first_hit_step, k-budget cap
  - Graph-usage probe: detects aethyme CLI, MCP tools, ordering vs edits
  - Skipped paths: no calls, no ground truth
  - Path matching: suffix-aware across absolute/relative paths
"""

from __future__ import annotations

from src.eval.probes import (
    ToolCall,
    run_all_probes,
    run_graph_usage_probe,
    run_navigation_probe,
)


def _call(order: int, name: str, **args) -> ToolCall:
    return ToolCall(order=order, name=name, arguments=dict(args))


# Navigation probe ---------------------------------------------------------

def test_navigation_perfect_hit() -> None:
    gt = {
        "must_open": ["includes/Page/WikiPage.php"],
        "must_not_open": [],
        "k_budget": 5,
    }
    calls = [_call(1, "Read", file_path="/mw/includes/Page/WikiPage.php")]
    r = run_navigation_probe(calls, gt)
    assert r["skipped"] is False
    assert r["first_hit_step"] == 1
    assert r["recall_at_k"] == 1.0
    assert r["distractor_rate"] == 0.0


def test_navigation_miss() -> None:
    gt = {
        "must_open": ["includes/Page/WikiPage.php"],
        "must_not_open": [],
        "k_budget": 3,
    }
    calls = [_call(1, "Read", file_path="/mw/some/unrelated.php")]
    r = run_navigation_probe(calls, gt)
    assert r["first_hit_step"] is None
    assert r["recall_at_k"] == 0.0


def test_navigation_suffix_match_absolute_path() -> None:
    """Agent uses an absolute path — match should still work."""
    gt = {
        "must_open": ["includes/Page/Article.php"],
        "must_not_open": [],
        "k_budget": 3,
    }
    calls = [_call(1, "Read",
                   file_path="/Users/foo/Playground/mediawiki/includes/Page/Article.php")]
    r = run_navigation_probe(calls, gt)
    assert r["first_hit_step"] == 1


def test_navigation_distractor_counted() -> None:
    gt = {
        "must_open": ["includes/Page/WikiPage.php"],
        "must_not_open": ["includes/Revision/RevisionStore.php"],
        "k_budget": 5,
    }
    calls = [
        _call(1, "Read", file_path="/mw/includes/Revision/RevisionStore.php"),
        _call(2, "Read", file_path="/mw/includes/Page/WikiPage.php"),
    ]
    r = run_navigation_probe(calls, gt)
    assert "includes/Revision/RevisionStore.php" in r["distractors"]
    assert r["first_hit_step"] == 2
    assert r["distractor_rate"] == 0.5


def test_navigation_k_budget_caps_examined_calls() -> None:
    gt = {
        "must_open": ["includes/Page/WikiPage.php"],
        "must_not_open": [],
        "k_budget": 2,
    }
    calls = [
        _call(1, "Read", file_path="/mw/noise1.php"),
        _call(2, "Read", file_path="/mw/noise2.php"),
        # Hit at step 3 — OUTSIDE budget, should not count
        _call(3, "Read", file_path="/mw/includes/Page/WikiPage.php"),
    ]
    r = run_navigation_probe(calls, gt)
    assert r["first_hit_step"] is None
    assert r["k_examined"] == 2


def test_navigation_grep_counts_as_nav() -> None:
    gt = {
        "must_open": ["rbac-canonical.ts"],
        "must_not_open": [],
        "k_budget": 5,
    }
    calls = [
        _call(1, "Grep", pattern="READ", path="/repo/packages/auth/src/rbac-canonical.ts"),
    ]
    r = run_navigation_probe(calls, gt)
    assert r["first_hit_step"] == 1


def test_navigation_skipped_when_no_nav_calls() -> None:
    gt = {"must_open": ["a.ts"], "must_not_open": [], "k_budget": 5}
    calls = [_call(1, "Bash", command="ls")]
    r = run_navigation_probe(calls, gt)
    assert r["skipped"] is True


# Graph-usage probe --------------------------------------------------------

def test_graph_usage_detects_aethyme_cli() -> None:
    calls = [
        _call(1, "Read", file_path="/mw/a.php"),
        _call(2, "Bash", command="aethyme find-facts --symbol doViewUpdates"),
        _call(3, "Edit", file_path="/mw/a.php", new_string="x"),
    ]
    r = run_graph_usage_probe(calls)
    assert r["aethyme_called"] is True
    assert r["first_aethyme_step"] == 2
    assert r["first_mutating_step"] == 3
    assert r["aethyme_called_before_first_edit"] is True


def test_graph_usage_detects_mcp_tool() -> None:
    calls = [
        _call(1, "aethyme_find_facts", symbol="foo"),
        _call(2, "Edit", file_path="/mw/a.php"),
    ]
    r = run_graph_usage_probe(calls)
    assert r["aethyme_called"] is True
    assert r["aethyme_call_count"] == 1


def test_graph_usage_detects_python_module_invocation() -> None:
    calls = [
        _call(1, "Bash", command="python -m src.cli find-facts --symbol x"),
    ]
    r = run_graph_usage_probe(calls)
    assert r["aethyme_called"] is True


def test_graph_usage_no_edit_returns_none_for_before_first_edit() -> None:
    """Explain-repo style evals that never edit → `before_first_edit` is None."""
    calls = [
        _call(1, "Bash", command="aethyme find-facts --symbol x"),
    ]
    r = run_graph_usage_probe(calls)
    assert r["aethyme_called_before_first_edit"] is None


def test_graph_usage_not_called_is_false() -> None:
    calls = [
        _call(1, "Read", file_path="/mw/a.php"),
        _call(2, "Edit", file_path="/mw/a.php"),
    ]
    r = run_graph_usage_probe(calls)
    assert r["aethyme_called"] is False
    assert r["aethyme_called_before_first_edit"] is False


def test_graph_usage_called_after_edit_is_false() -> None:
    """Using the graph *after* editing doesn't count as informed navigation."""
    calls = [
        _call(1, "Edit", file_path="/mw/a.php"),
        _call(2, "Bash", command="aethyme find-facts --symbol x"),
    ]
    r = run_graph_usage_probe(calls)
    assert r["aethyme_called"] is True
    assert r["aethyme_called_before_first_edit"] is False


def test_graph_usage_skipped_when_empty() -> None:
    r = run_graph_usage_probe([])
    assert r["skipped"] is True


# Dispatcher ---------------------------------------------------------------

def test_run_all_probes_returns_both() -> None:
    gt = {"must_open": ["a.ts"], "must_not_open": [], "k_budget": 5}
    calls = [
        _call(1, "Read", file_path="/r/a.ts"),
        _call(2, "Bash", command="aethyme navigate a.ts"),
    ]
    r = run_all_probes(calls, gt)
    assert r["navigation"]["first_hit_step"] == 1
    assert r["graph_usage"]["aethyme_called"] is True


def test_run_all_probes_no_ground_truth_skips_navigation() -> None:
    calls = [_call(1, "Read", file_path="/r/a.ts")]
    r = run_all_probes(calls, None)
    assert r["navigation"]["skipped"] is True
    assert r["graph_usage"]["skipped"] is False


if __name__ == "__main__":
    tests = [v for k, v in list(globals().items()) if k.startswith("test_")]
    failed = 0
    for t in tests:
        try:
            t()
            print(f"PASS {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"FAIL {t.__name__}: {e}")
        except Exception as e:
            failed += 1
            print(f"ERROR {t.__name__}: {type(e).__name__}: {e}")
    raise SystemExit(1 if failed else 0)
