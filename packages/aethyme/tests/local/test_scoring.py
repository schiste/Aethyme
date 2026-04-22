"""Unit tests for eval scoring helpers.

Determinism contract: every assertion here must produce the same result
regardless of platform, Python version, or repo under test.  Floating-point
comparisons use ``pytest.approx`` except where the result is exactly 0.0 or
1.0 (which are representable without error).
"""

from __future__ import annotations

import pytest

from src.eval.schemas import (
    EXPLAIN_REPO_PATH_KEYS,
    NAVIGATION_CTF_PATH_KEYS,
    mediawiki_bug_fix_1_reference,
)
from src.eval.scoring import (
    _exact_path_score,
    _extract_path_strings,
    _list_score,
    _normalize_path,
    _ordered_list_score,
    _relationship_chain_score,
    parse_structured_output,
    score_dead_code,
    score_explain_repo_output,
    score_mediawiki_bug_fix_1,
    score_navigation_ctf_output,
)

# Synthetic repo root used across all tests.  NOT a real path on disk —
# _normalize_path only does string prefix stripping, no filesystem access.
REPO = "/synthetic/test/repo"


# ── _normalize_path ──────────────────────────────────────────────────────


class TestNormalizePath:
    def test_identity(self):
        assert _normalize_path("src/main.py") == "src/main.py"

    def test_absolute_with_repo_path(self):
        assert _normalize_path(f"{REPO}/src/main.py", REPO) == "src/main.py"

    def test_absolute_without_repo_path(self):
        assert _normalize_path(f"{REPO}/src/main.py") == f"{REPO}/src/main.py"

    def test_markdown_link_file_url(self):
        assert _normalize_path("[cli.py](src/cli.py)") == "src/cli.py"

    def test_markdown_link_with_line_anchor(self):
        assert _normalize_path("[cli.py](src/cli.py#L159)") == "src/cli.py"

    def test_markdown_link_http_url_uses_text(self):
        assert _normalize_path("[src/cli.py](https://github.com/x/y)") == "src/cli.py"

    def test_line_anchor_stripped(self):
        assert _normalize_path("src/cli.py#L159") == "src/cli.py"

    def test_line_range_stripped(self):
        assert _normalize_path("src/cli.py#L10-L20") == "src/cli.py"

    def test_leading_dot_slash(self):
        assert _normalize_path("./src/cli.py") == "src/cli.py"

    def test_double_leading_dot_slash(self):
        assert _normalize_path("././src/cli.py") == "src/cli.py"

    def test_trailing_slash(self):
        assert _normalize_path("src/") == "src"

    def test_combined(self):
        assert _normalize_path("./src/file.py#L1") == "src/file.py"

    def test_empty_string(self):
        assert _normalize_path("") == ""

    def test_non_string(self):
        assert _normalize_path(42) == 42  # type: ignore[arg-type]

    def test_absolute_markdown_with_repo_path(self):
        full = f"[main]({REPO}/src/main.py#L5)"
        assert _normalize_path(full, REPO) == "src/main.py"

    def test_repo_path_with_trailing_slash(self):
        assert _normalize_path(f"{REPO}/src/x.py", REPO + "/") == "src/x.py"


class TestParseStructuredOutput:
    def test_plain_json_object(self):
        payload = parse_structured_output('{"files_to_edit": [], "root_cause": "x", "fix_plan": "y", "testing": "z"}')
        assert payload is not None
        assert payload["root_cause"] == "x"

    def test_fenced_json_object(self):
        payload = parse_structured_output(
            """Here is the result:

```json
{"files_to_edit": [], "root_cause": "x", "fix_plan": "y", "testing": "z"}
```
"""
        )
        assert payload is not None
        assert payload["testing"] == "z"

    def test_dead_code_payload(self):
        payload = parse_structured_output(
            '{"unused_functions":[{"function_name":"duplicateEntry","defined_in":"includes/Watchlist/WatchedItemStore.php","reason":"no callers found"}]}'
        )
        assert payload is not None
        assert payload["unused_functions"][0]["function_name"] == "duplicateEntry"


# ── _exact_path_score ────────────────────────────────────────────────────


class TestExactPathScore:
    def test_bare_match(self):
        assert _exact_path_score("src/a.py", "src/a.py") == 1.0

    def test_mismatch(self):
        assert _exact_path_score("src/b.py", "src/a.py") == 0.0

    def test_absolute_with_repo_path(self):
        assert _exact_path_score(f"{REPO}/src/a.py", "src/a.py", repo_path=REPO) == 1.0

    def test_markdown_with_repo_path(self):
        assert _exact_path_score(f"[a]({REPO}/src/a.py#L1)", "src/a.py", repo_path=REPO) == 1.0

    def test_empty_reference(self):
        assert _exact_path_score("anything", "") == 1.0

    def test_none_reference(self):
        assert _exact_path_score("anything", None) == 1.0


# ── _list_score ──────────────────────────────────────────────────────────


class TestListScore:
    def test_exact(self):
        assert _list_score(["a", "b"], ["a", "b"]) == 1.0

    def test_partial(self):
        assert _list_score(["a"], ["a", "b"]) == 0.5

    def test_superset(self):
        assert _list_score(["a", "b", "c"], ["a", "b"]) == 1.0

    def test_empty_reference(self):
        assert _list_score([], []) == 1.0

    def test_normalized_absolute(self):
        score = _list_score(
            [f"{REPO}/src/a.py", f"{REPO}/src/b.py"],
            ["src/a.py", "src/b.py"],
            repo_path=REPO,
        )
        assert score == 1.0

    def test_mixed_types_in_list(self):
        """Non-string items in the list are silently dropped."""
        score = _list_score(["a", 42, None, "b"], ["a", "b"])
        assert score == 1.0


# ── _ordered_list_score ──────────────────────────────────────────────────


class TestOrderedListScore:
    def test_correct_order(self):
        assert _ordered_list_score(["a", "b", "c"], ["a", "b", "c"]) == 1.0

    def test_wrong_order(self):
        score = _ordered_list_score(["b", "a", "c"], ["a", "b", "c"])
        # a: present but wrong position → 0.5
        # b: present but wrong position → 0.5
        # c: correct position → 1.0
        # total = 2.0 / 3
        assert score == pytest.approx(2.0 / 3)

    def test_missing(self):
        score = _ordered_list_score(["a"], ["a", "b"])
        # a: correct position → 1.0; b: absent → 0.0
        assert score == pytest.approx(1.0 / 2)

    def test_normalized(self):
        score = _ordered_list_score(
            [f"{REPO}/a", f"{REPO}/b"],
            ["a", "b"],
            repo_path=REPO,
        )
        assert score == 1.0


# ── _relationship_chain_score ────────────────────────────────────────────


class TestRelationshipChainScore:
    def test_exact(self):
        chain = [{"from": "a.json", "to": "b.py", "relation": "entrypoint_for"}]
        assert _relationship_chain_score(chain, chain) == 1.0

    def test_normalized(self):
        candidate = [{"from": f"{REPO}/a.json", "to": f"{REPO}/b.py", "relation": "entrypoint_for"}]
        reference = [{"from": "a.json", "to": "b.py", "relation": "entrypoint_for"}]
        assert _relationship_chain_score(candidate, reference, repo_path=REPO) == 1.0

    def test_over_inclusion(self):
        candidate = [
            {"from": "a.json", "to": "b.py", "relation": "entrypoint_for"},
            {"from": "a.json", "to": "c.py", "relation": "entrypoint_for"},
        ]
        reference = [{"from": "a.json", "to": "b.py", "relation": "entrypoint_for"}]
        assert _relationship_chain_score(candidate, reference) == 1.0

    def test_partial(self):
        candidate = [{"from": "a.json", "to": "b.py", "relation": "entrypoint_for"}]
        reference = [
            {"from": "a.json", "to": "b.py", "relation": "entrypoint_for"},
            {"from": "a.json", "to": "area", "relation": "configures"},
        ]
        assert _relationship_chain_score(candidate, reference) == 0.5

    def test_none_reference(self):
        """None reference means 'not tested' — consistent with other scorers."""
        candidate = [{"from": "a.json", "to": "b.py", "relation": "entrypoint_for"}]
        assert _relationship_chain_score(candidate, None) == 1.0

    def test_empty_reference(self):
        assert _relationship_chain_score([], []) == 1.0

    def test_none_candidate_with_reference(self):
        reference = [{"from": "a.json", "to": "b.py", "relation": "entrypoint_for"}]
        assert _relationship_chain_score(None, reference) == 0.0


class TestScoreDeadCode:
    def test_generic_dead_code_scoring(self):
        reference = {
            "unused_functions": [
                {"function_name": "activate"},
                {"function_name": "workspace_inspect"},
            ]
        }
        candidate = {
            "unused_functions": [
                {"function_name": "activate"},
                {"function_name": "false_positive"},
            ]
        }

        result = score_dead_code(candidate, reference, cost_usd=0.5)

        assert result["scores"]["functions_found"] == pytest.approx(0.5)
        assert result["scores"]["false_positives"] == pytest.approx(0.5)
        assert result["weighted_score"] > 0


# ── _extract_path_strings ───────────────────────────────────────────────


class TestExtractPathStrings:
    def test_no_allowlist_collects_all(self):
        """Legacy mode (path_keys=None) collects every string."""
        data = {"code_areas": ["src/", "lib/"], "repo_summary": "prose"}
        paths = _extract_path_strings(data, path_keys=None)
        assert "src/" in paths
        assert "lib/" in paths
        assert "prose" in paths

    def test_explain_repo_allowlist_skips_prose(self):
        data = {"code_areas": ["src/", "lib/"], "repo_summary": "skip me"}
        paths = _extract_path_strings(data, EXPLAIN_REPO_PATH_KEYS)
        assert "src/" in paths
        assert "lib/" in paths
        assert "skip me" not in paths

    def test_explain_repo_allowlist_skips_evidence(self):
        data = {"evidence": ["some citation"], "entrypoints": ["main.py"]}
        paths = _extract_path_strings(data, EXPLAIN_REPO_PATH_KEYS)
        assert "main.py" in paths
        assert "some citation" not in paths

    def test_explain_repo_allowlist_skips_key_languages(self):
        data = {"key_languages": ["python", "rust"], "key_configs": ["Cargo.toml"]}
        paths = _extract_path_strings(data, EXPLAIN_REPO_PATH_KEYS)
        assert "Cargo.toml" in paths
        assert "python" not in paths

    def test_ctf_allowlist_nested(self):
        data = {"config_target": {"path": "pkg.json", "why": "skip"}}
        paths = _extract_path_strings(data, NAVIGATION_CTF_PATH_KEYS)
        assert "pkg.json" in paths
        assert "skip" not in paths

    def test_ctf_allowlist_skips_relation(self):
        data = {
            "relationship_chain": [
                {"from": "a.json", "to": "b.py", "relation": "entrypoint_for"}
            ]
        }
        paths = _extract_path_strings(data, NAVIGATION_CTF_PATH_KEYS)
        assert "a.json" in paths
        assert "b.py" in paths
        assert "entrypoint_for" not in paths

    def test_ctf_allowlist_skips_confidence(self):
        data = {"confidence": "high", "config_target": {"path": "x.json", "why": "y"}}
        paths = _extract_path_strings(data, NAVIGATION_CTF_PATH_KEYS)
        assert "x.json" in paths
        assert "high" not in paths
        assert "y" not in paths

    def test_ctf_allowlist_skips_rejected_reason(self):
        data = {"rejected_candidates": [{"path": "a.json", "reason": "wrong"}]}
        paths = _extract_path_strings(data, NAVIGATION_CTF_PATH_KEYS)
        assert "a.json" in paths
        assert "wrong" not in paths

    def test_allowlist_recurses_through_non_path_parent(self):
        """Leaf path key inside a non-allowlisted parent is still collected."""
        data = {"unknown_wrapper": {"path": "found.py", "why": "ignored"}}
        paths = _extract_path_strings(data, NAVIGATION_CTF_PATH_KEYS)
        assert "found.py" in paths
        assert "ignored" not in paths


# ── Integration: score_explain_repo_output ───────────────────────────────


class TestScoreExplainRepoOutput:
    def _make_reference(self):
        return {
            "repo_summary": "A demo repo",
            "code_areas": ["src/"],
            "reference_areas": ["docs/"],
            "entrypoints": ["src/main.py"],
            "important_docs": ["README.md"],
            "key_configs": ["package.json"],
            "key_languages": ["python"],
            "high_risk_areas": ["auth/"],
            "navigation_order": ["README.md", "src/"],
            "representative_code_files": ["src/main.py"],
            "representative_docs": ["README.md"],
        }

    def test_perfect_score(self):
        ref = self._make_reference()
        result = score_explain_repo_output(ref, ref)
        assert result is not None
        assert result["weighted_score"] == 100.0

    def test_none_candidate(self):
        assert score_explain_repo_output(None, self._make_reference()) is None

    def test_absolute_paths_without_normalization(self):
        ref = self._make_reference()
        # Prepends REPO to every list item — including key_languages.
        candidate = {
            k: [f"{REPO}/{v}" for v in vals] if isinstance(vals, list) else vals
            for k, vals in ref.items()
        }
        result = score_explain_repo_output(candidate, ref)
        assert result is not None
        # Without repo_path, no prefix stripping happens.  Every list field
        # is mangled (even key_languages: "python" → "/synthetic/.../python"),
        # so nothing matches.
        assert result["weighted_score"] == 0.0

    def test_absolute_paths_with_normalization(self):
        ref = self._make_reference()
        candidate = {
            k: [f"{REPO}/{v}" for v in vals] if isinstance(vals, list) else vals
            for k, vals in ref.items()
        }
        result = score_explain_repo_output(candidate, ref, repo_path=REPO)
        assert result is not None
        assert result["weighted_score"] == 100.0

    def test_guardrails_present(self):
        ref = self._make_reference()
        result = score_explain_repo_output(ref, ref)
        assert result is not None
        assert "guardrails" in result
        g = result["guardrails"]
        assert "path_format_issues" in g
        assert "raw_score" in g
        assert "normalized_score" in g
        assert "normalization_delta" in g

    def test_guardrails_delta_positive_with_normalization(self):
        ref = self._make_reference()
        candidate = {
            k: [f"{REPO}/{v}" for v in vals] if isinstance(vals, list) else vals
            for k, vals in ref.items()
        }
        result = score_explain_repo_output(candidate, ref, repo_path=REPO)
        assert result is not None
        assert result["guardrails"]["normalization_delta"] > 0


# ── Integration: score_navigation_ctf_output ─────────────────────────────


class TestScoreNavigationCtfOutput:
    def _make_reference(self):
        return {
            "config_target": {"path": "auth/package.json", "why": "test"},
            "code_target": {"path": "ui/src/index.ts", "why": "test"},
            "management_area": {"name": "packages", "why": "test"},
            "relationship_chain": [
                {"from": "auth/package.json", "to": "ui/src/index.ts", "relation": "entrypoint_for"},
                {"from": "auth/package.json", "to": "packages", "relation": "configures"},
            ],
            "rejected_candidates": [],
            "confidence": "high",
        }

    def test_perfect_score(self):
        ref = self._make_reference()
        result = score_navigation_ctf_output(ref, ref)
        assert result is not None
        assert result["weighted_score"] == 100.0

    def test_none_candidate(self):
        assert score_navigation_ctf_output(None, self._make_reference()) is None

    def test_absolute_paths_with_normalization(self):
        ref = self._make_reference()
        candidate = {
            "config_target": {"path": f"{REPO}/auth/package.json", "why": "test"},
            "code_target": {"path": f"{REPO}/ui/src/index.ts", "why": "test"},
            "management_area": {"name": "packages", "why": "test"},
            "relationship_chain": [
                {"from": f"{REPO}/auth/package.json", "to": f"{REPO}/ui/src/index.ts", "relation": "entrypoint_for"},
                {"from": f"{REPO}/auth/package.json", "to": "packages", "relation": "configures"},
            ],
            "rejected_candidates": [],
            "confidence": "high",
        }
        result = score_navigation_ctf_output(candidate, ref, repo_path=REPO)
        assert result is not None
        assert result["weighted_score"] == 100.0

    def test_guardrails_present(self):
        ref = self._make_reference()
        result = score_navigation_ctf_output(ref, ref)
        assert result is not None
        assert "guardrails" in result


class TestScoreMediawikiBugFix1:
    def test_scores_structured_candidate_with_testing_field(self):
        reference = mediawiki_bug_fix_1_reference()
        candidate = {
            "files_to_edit": [
                {
                    "path": "includes/Page/WikiPage.php",
                    "what_to_change": "Change doViewUpdates to accept a RevisionRecord and add a deprecation shim.",
                },
                {
                    "path": "includes/Page/Article.php",
                    "what_to_change": "Update the showDiffPage/doViewUpdates call sites to pass a RevisionRecord.",
                },
                {
                    "path": "includes/Page/ImagePage.php",
                    "what_to_change": "Use fetchRevisionRecord instead of getOldID.",
                },
                {
                    "path": "RELEASE-NOTES-1.46",
                    "what_to_change": "Document the deprecation of passing oldid to doViewUpdates.",
                },
            ],
            "root_cause": (
                "showDiffPage passes an integer oldid into doViewUpdates, but "
                "WatchlistManager::clearTitleUserNotifications needs a RevisionRecord "
                "for the viewed revision."
            ),
            "fix_plan": (
                "Change the doViewUpdates signature, keep a func_num_args deprecation "
                "shim, and update callers to pass fetchRevisionRecord/getNewRevision."
            ),
            "testing": (
                "Verify diff and revision views only mark the correct revision as seen, "
                "and add a regression around watchlist notification clearing."
            ),
        }

        result = score_mediawiki_bug_fix_1(candidate, reference, cost_usd=0.2, repo_path=REPO)

        assert result["scores"]["files_identified"] == 1.0
        assert result["scores"]["testing_quality"] > 0.0
        assert result["weighted_score"] > 70.0
        assert result["candidate"]["testing"].startswith("Verify diff")


# ── Regression: CTO-off rerun data ──────────────────────────────────────

# These tests pin known results from the 2026-03-09 CTO-off control rerun.
# The expected scores are derived from the weight definitions, NOT hardcoded
# magic numbers.  If weights change, these tests update automatically.

_CTF_WEIGHTS = {
    "config_target": 30,
    "code_target": 30,
    "management_area": 20,
    "relationship_chain": 20,
}


def _make_ctf_reference():
    return {
        "config_target": {"path": "packages/auth/package.json", "why": "ref"},
        "code_target": {"path": "packages/ui/src/tokens/index.ts", "why": "ref"},
        "management_area": {"name": "packages", "why": "ref"},
        "relationship_chain": [
            {"from": "packages/auth/package.json", "to": "packages/ui/src/tokens/index.ts", "relation": "entrypoint_for"},
            {"from": "packages/auth/package.json", "to": "packages", "relation": "configures"},
        ],
        "rejected_candidates": [],
        "confidence": "high",
    }


class TestRegressionCtoOff:
    """Verify known scores from the CTO-off control rerun.

    The rerun candidate used absolute paths and picked different config/code
    targets than the reference.  Only ``management_area`` matched, so the
    raw score equals the ``management_area`` weight.
    """

    # The candidate picked wrong config/code but correct area.
    # Score = management_area weight (only matching field).
    EXPECTED_RAW_SCORE = float(_CTF_WEIGHTS["management_area"])

    def test_raw_score_matches_known(self):
        reference = _make_ctf_reference()
        # CTO-off rerun candidate: absolute paths, wrong config/code targets
        candidate = {
            "config_target": {"path": f"{REPO}/packages/ui/package.json", "why": "test"},
            "code_target": {"path": f"{REPO}/packages/ui/src/index.ts", "why": "test"},
            "management_area": {"name": "packages", "why": "test"},
            "relationship_chain": [
                {"from": f"{REPO}/packages/ui/package.json", "to": f"{REPO}/packages/ui/src/index.ts", "relation": "entrypoint_for"},
            ],
            "rejected_candidates": [],
            "confidence": "high",
        }
        raw_result = score_navigation_ctf_output(candidate, reference)
        assert raw_result is not None
        assert raw_result["guardrails"]["raw_score"] == pytest.approx(self.EXPECTED_RAW_SCORE)

    def test_normalization_delta_positive(self):
        """With repo_path, stripping absolute prefix should increase the score."""
        reference = _make_ctf_reference()
        # Candidate with CORRECT answers but absolute paths
        candidate = {
            "config_target": {"path": f"{REPO}/packages/auth/package.json", "why": "test"},
            "code_target": {"path": f"{REPO}/packages/ui/src/tokens/index.ts", "why": "test"},
            "management_area": {"name": "packages", "why": "test"},
            "relationship_chain": [
                {"from": f"{REPO}/packages/auth/package.json", "to": f"{REPO}/packages/ui/src/tokens/index.ts", "relation": "entrypoint_for"},
                {"from": f"{REPO}/packages/auth/package.json", "to": "packages", "relation": "configures"},
            ],
            "rejected_candidates": [],
            "confidence": "high",
        }
        result = score_navigation_ctf_output(candidate, reference, repo_path=REPO)
        assert result is not None
        assert result["guardrails"]["normalization_delta"] > 0
        assert result["weighted_score"] == 100.0
