"""Tests for the cross-process contract check script.

The script at `scripts/check-cross-process-contract.py` is the
friction layer for the cardinal rule that deleting a cross-process
entry point must be deliberate. We test the parsing units directly
(extracting tracked symbols, finding diff hits, parsing the
contract decision) — the orchestration around `git diff` is left to
manual smoke tests because exercising it deterministically would
require seeding a git repo per test, which isn't worth the
complexity for a single shell-glue function.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[4]
SCRIPT_PATH = REPO_ROOT / "scripts" / "check-cross-process-contract.py"


def _load_script_module():
    """Import the script as a module despite its hyphenated filename
    not being a valid Python identifier."""
    spec = importlib.util.spec_from_file_location("cpcc", SCRIPT_PATH)
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture(scope="module")
def cpcc():
    return _load_script_module()


def test_script_exists_and_is_executable():
    """The CI workflow runs the script directly. If the file goes
    missing or loses its shebang, CI silently fails to invoke it."""
    assert SCRIPT_PATH.exists()
    head = SCRIPT_PATH.read_text(encoding="utf-8").splitlines()[0]
    assert head.startswith("#!"), f"missing shebang: {head!r}"


def test_extract_tracked_symbols_strips_short_and_noisy_tokens(cpcc):
    """The MIN_SYMBOL_LEN cutoff drops tokens like `a`, `it`, `of`
    that would otherwise produce thousands of false positives in
    diffs. Symbols 4 chars or longer make it through."""
    sample = (
        "First, see `foo` (too short — dropped).\n"
        "Second, `aethyme-explore` is tracked.\n"
        "Third, `usage_boundary_query` is tracked.\n"
        "Bare numbers `2024` should be dropped (excluded).\n"
        "ALLCAPS `TODO` should be dropped (excluded).\n"
    )
    tracked = cpcc.extract_tracked_symbols(sample)
    assert "aethyme-explore" in tracked
    assert "usage_boundary_query" in tracked
    assert "foo" not in tracked
    assert "2024" not in tracked
    assert "TODO" not in tracked


def test_find_touched_symbols_only_flags_removals(cpcc):
    """Added lines mentioning a tracked symbol are usually new
    consumers — they don't break things by being added. Only
    removed lines are dangerous, so the function ignores additions
    and unchanged lines."""
    tracked = {"aethyme-explore"}
    diff = [
        "--- a/foo",
        "+++ b/foo",
        "+ adding aethyme-explore (this is fine)",
        "  context line mentions aethyme-explore (unchanged)",
        "- removing aethyme-explore (this is the dangerous direction)",
    ]
    findings = cpcc.find_touched_symbols(diff, tracked)
    assert "aethyme-explore" in findings
    assert len(findings["aethyme-explore"]) == 1
    assert findings["aethyme-explore"][0].startswith("-")


def test_find_touched_symbols_skips_diff_headers(cpcc):
    """Lines like `--- a/foo` start with `--` and would otherwise
    be misclassified as removals. The function must skip them."""
    tracked = {"aethyme-explore"}
    diff = [
        "--- a/aethyme-explore",   # header — should be skipped
        "+++ b/aethyme-explore",   # header — should be skipped
        "-some other line",
    ]
    findings = cpcc.find_touched_symbols(diff, tracked)
    assert findings == {}, (
        f"diff headers leaked through as findings: {findings}"
    )


def test_parse_contract_decision_picks_checked_label(cpcc):
    body = "- [x] **soft-retire** — deprecated"
    assert cpcc.parse_contract_decision(body) == "soft-retire"


def test_parse_contract_decision_returns_none_when_unchecked(cpcc):
    body = "- [ ] **none**\n- [ ] **introduce**"
    assert cpcc.parse_contract_decision(body) is None


def test_parse_contract_decision_prefers_most_restrictive_when_multiple(cpcc):
    """If a contributor checks multiple labels (mistake or
    indecision), prefer the most-restrictive label so the check
    doesn't get fooled by a co-checked `none`."""
    body = (
        "- [x] **none**\n"
        "- [x] **hard-delete**\n"
    )
    assert cpcc.parse_contract_decision(body) == "hard-delete"


def test_parse_contract_decision_handles_empty_body(cpcc):
    assert cpcc.parse_contract_decision("") is None
    assert cpcc.parse_contract_decision(None) is None  # type: ignore[arg-type]


def test_consumers_doc_yields_real_tracked_symbols(cpcc):
    """End-to-end: feeding the actual cross-process-consumers.md
    must produce a sane tracked set including the high-blast-radius
    names. If this set is empty or tiny the script silently no-ops
    in CI."""
    tracked = cpcc.extract_tracked_symbols(cpcc.CONSUMERS_DOC.read_text())
    assert len(tracked) >= 20, (
        f"tracked set suspiciously small: {len(tracked)}"
    )
    # Spot-check a couple of known high-blast-radius entry points.
    assert any("aethyme-explore" in s for s in tracked)
    assert any("SKILL.md" in s for s in tracked)
